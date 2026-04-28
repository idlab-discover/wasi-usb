# CHANGES — implementatie-log

> Dit bestand documenteert alle wijzigingen die aangebracht zijn na de
> initiële C1–C5 benchmark-implementatie (F1–F7).  Het beslaat twee
> onderwerpen:
>
> - **Robustere meetharnas** (bench/run.sh, logging)
> - **Bug-fixes voor correcte werking van C2/C4/C5 over meerdere iteraties**
>   (LIBUSB\_ERROR\_BUSY, double-free, WASI-bestandssysteem)

---

## 1. bench/run.sh — harness-robuustheid

### 1.1 Niet meer afbreken bij device-not-found

**Probleem:** `run_cmd` gebruikte `eval "$@"` zonder foutafhandeling.
Bij een niet-aangesloten apparaat brak het script af op de eerste cel.

**Oplossing:**
```bash
run_cmd() {
    echo "    \$ $*"
    if [[ $DRY_RUN -eq 0 ]]; then
        eval "$@" || {
            local rc=$?
            echo "  [WARN] command exited with status ${rc} — device may not be connected"
            return ${rc}
        }
    fi
}
```
`warm_cell` en `run_cell` worden in de hoofdlus aangeroepen met `|| true`,
zodat één mislukte cel de rest van de matrix niet blokkeert.

### 1.2 Correcte VID:PID voor aangesloten apparaten

De standaard-VID:PIDs zijn bijgewerkt naar de daadwerkelijk aangesloten
hardware (bepaald met `just lsusb`):

| Workload | Oud | Nieuw | Apparaat |
|----------|-----|-------|----------|
| bulk | `0951:1666` | `0781:5581` | SanDisk 3.2Gen1 |
| ctrl | `2341:8057` | `cafe:4002` | WASI-USB Loopback |
| iso  | `046d:086c` | `046d:094c` | Logitech Brio 100 |
| int  | `054c:0ce6` | `054c:0ce6` | PS5 DualSense (ongewijzigd) |

### 1.3 WASI-bestandssysteem: relatieve CSV-paden

**Probleem:** WASI-componenten (C2/C4/C5) werden gestart vanuit de
projectroot, maar ontvingen het absolute pad naar het CSV-bestand (bijv.
`/Users/.../results/.../bulk_C2.csv`). De host preopent alleen `.` = `REPO_ROOT`
als virtuele directory; absolute paden buiten die sandbox zijn ontoegankelijk.
Resultaat: `bench_csv_open: No such file or directory`.

**Oplossing:** relatief pad berekenen en cwd vastzetten:
```bash
local rel_csv="${out_csv#${REPO_ROOT}/}"

run_cmd cd "\"${REPO_ROOT}\"" "&&" \
    ${SUDO} ${rt} "\"${HOST}\"" -c "\"${wasm}\"" -- \
    "\"${rel_csv}\"" "\"${vidpid}\"" "\"${iters}\"" \
    --condition wasi-libusb
```
`rel_csv` bevat nu `results/<ts>/bulk_C2.csv` — een pad relatief aan
`REPO_ROOT`, dat wél beschikbaar is in de WASI-sandbox.

### 1.4 Automatisch unmounten van USB-stick (macOS, bulk-workload)

Op macOS koppelt het OS een USB-massaopslagapparaat automatisch aan en
claimt het `IOUSBMassStorageClass`-stuurprogramma de interface. libusb
kan de interface dan niet overnemen. Het script probeert nu automatisch
te unmounten:

```bash
if [[ "${wl}" == "bulk" && "$(uname -s)" == "Darwin" ]]; then
    disk=$(system_profiler SPUSBDataType | awk -v v="${vid}" -v p="${pid}" '...')
    ${SUDO} diskutil unmount "/dev/${disk}"
fi
```

---

## 2. usb-wasi-host — logging

### 2.1 Per-transfer logs verlaagd naar DEBUG

**Probleem:** Elk transfer-event produceerde een `info!`-logregel (→ stderr-write
→ syscall). Bij 1 000 iteraties met 6 `info!`-regels per transfer = 6 000 extra
syscalls, gemeten als latency-verhoging in de benchmarks.

**Gewijzigde bestanden:** `usb-wasi-host/src/main.rs` en
`usb-wasi-host/src/usb_backend.rs`.

| Was | Nu |
|-----|----|
| `info!("transfer_callback fired, status: {}")` | `debug!()` |
| `info!("ISO transfer received {} bytes")` | `debug!()` |
| `info!("IN transfer")` / `info!("OUT transfer")` | `debug!()` |
| `info!("Awaiting transfer")` | `debug!()` |
| `info!("Transfer completed, {} bytes")` | `debug!()` |
| `info!("Starting new_transfer …")` | `debug!()` |
| `info!("Transfer resource created successfully")` | `debug!()` |
| `info!("list_devices backend called.")` | `debug!()` |
| `println!("[HOST] processing device …")` e.a. | `debug!()` |
| `info!("libusb_claim_interface successful …")` | `debug!()` |

Aan `INFO`-niveau gebleven (eenmalig per run):
- `info!("Starting WASM component")`
- `info!("Backend initialized")`
- `info!("WASM component finished")`

---

## 3. Bug: LIBUSB\_ERROR\_BUSY bij isochrone transfers (C2/C4)

### 3.1 Root cause

De C-benchmark `w_iso.c` hergebruikt **één** `libusb_transfer`-struct over alle
iteraties (standaard libusb-patroon: `libusb_alloc_transfer` buiten de lus,
`libusb_submit_transfer` binnen de lus).

In de WASI-backend (`libusb/libusb/os/wasi_usb.c`) was
`wasm_submit_transfer` volledig **synchroon**: het riep zowel `submit_transfer`
als `await_transfer` (WIT) aan en signaleerde daarna onmiddellijk de
voltooiing met `usbi_handle_transfer_completion()`.

Het probleem zit in de **volgorde** in `libusb/libusb/io.c`:

```
libusb_submit_transfer():
  1. state_flags = 0          ← wist IN_FLIGHT
  2. wasm_submit_transfer():  ← synchroon!
       a. WIT new_transfer / submit / await
       b. usbi_handle_transfer_completion()  ← wist IN_FLIGHT
       c. iso_callback() → completed = 1
  3. state_flags |= IN_FLIGHT  ← ZET IN_FLIGHT opnieuw, ná return backend
```

Na stap 3 is `IN_FLIGHT` gezet, maar de transfer is al klaar. De
event-loop (`libusb_handle_events_completed`) doet niets nuttig in WASI
(geen echte file descriptors), dus `IN_FLIGHT` wordt nooit meer gewist.

Iteratie 1: `libusb_submit_transfer` ziet `IN_FLIGHT` → `LIBUSB_ERROR_BUSY`.

### 3.2 Fix in `libusb/libusb/os/wasi_usb.c`

**Principe:** `usbi_handle_transfer_completion()` mag pas worden aangeroepen
*nadat* `libusb_submit_transfer` de `IN_FLIGHT`-vlag gezet heeft. De oplossing
is de afhandeling te **deferren** naar `wasm_handle_events`.

#### Nieuw globaal veld

```c
static volatile int wasi_pending_completions = 0;
```

#### `wasm_submit_transfer` (vereenvoudigd diff)

```c
// VOOR (verkeerd):
itransfer->state_flags |= USBI_TRANSFER_IN_FLIGHT;  // overbodig, core doet dit
// ... WIT await ...
usbi_handle_transfer_completion(itransfer, LIBUSB_TRANSFER_COMPLETED);  // te vroeg!
return LIBUSB_SUCCESS;

// NA (correct):
// Geen IN_FLIGHT zetten — de libusb-core doet dit na return.
// Data kopiëren naar transfer->buffer (zelfde code als voorheen).
tpriv->completed = 1;
tpriv->transfer_status = LIBUSB_TRANSFER_COMPLETED;  // of TIMED_OUT/ERROR
wasi_pending_completions++;
return LIBUSB_SUCCESS;  // callback wordt gefired vanuit wasm_handle_events
```

#### `usbi_wait_for_events`

```c
if (wasi_pending_completions > 0) {
    reported_events->num_ready = wasi_pending_completions;
    reported_events->event_data = ctx->event_data;
    reported_events->event_data_count = ctx->event_data_cnt;
    return LIBUSB_SUCCESS;  // → libusb-core roept handle_events aan
}
```

#### `wasm_handle_events` (herschreven)

```c
restart:
    usbi_mutex_lock(&ctx->flying_transfers_lock);
    list_for_each_entry(itransfer, &ctx->flying_transfers, list, struct usbi_transfer) {
        wasi_transfer_priv_t *tpriv = get_transfer_priv(itransfer);
        if (tpriv->completed) {
            enum libusb_transfer_status status = tpriv->transfer_status;
            tpriv->completed = 0;
            if (wasi_pending_completions > 0) wasi_pending_completions--;
            usbi_mutex_unlock(&ctx->flying_transfers_lock);
            usbi_handle_transfer_completion(itransfer, status); // ← nu wél na IN_FLIGHT
            goto restart;
        }
    }
    usbi_mutex_unlock(&ctx->flying_transfers_lock);
```

#### `wasi_transfer_priv_t` uitgebreid (wasi_usb.h)

```c
typedef struct {
    // ... bestaande velden ...
    int transfer_status;  // libusb_transfer_status voor uitgestelde afhandeling
} wasi_transfer_priv_t;
```

### 3.3 Correcte tijdlijn na fix

```
libusb_submit_transfer():
  1. state_flags = 0
  2. wasm_submit_transfer():
       a. WIT new_transfer / submit / await
       b. data kopiëren naar transfer->buffer
       c. tpriv->completed = 1; wasi_pending_completions++
       d. return LIBUSB_SUCCESS
  3. state_flags |= IN_FLIGHT  ← gezet VOOR dat handle_events draait

libusb_handle_events_completed() (C event-loop):
  → usbi_wait_for_events: wasi_pending_completions > 0 → return SUCCESS
  → wasm_handle_events:
       usbi_handle_transfer_completion(...) ← wist IN_FLIGHT, roept iso_callback aan
       iso_callback: completed = 1

Iteratie 1: IN_FLIGHT is gewist → geen BUSY meer ✓
```

---

## 4. usb-wasi-host — correcte `drop` voor `UsbTransfer`

### 4.1 Originele bug

```rust
fn drop(&mut self, self_: Resource<UsbTransfer>) -> Result<(), Error> {
    // BUG: table.get() in plaats van table.delete() → resource lekt in tabel
    if let Ok(transfer) = self.table.get(&self_) {
        unsafe {
            if !transfer.completed.load(Ordering::SeqCst) {
                let _ = libusb_cancel_transfer(transfer.transfer);
            }
        }
    }
    Ok(())
    // BUG: libusb_free_transfer() nooit aangeroepen
    // BUG: table.delete() nooit aangeroepen → resource-tabel groeit onbeperkt
}
```

### 4.2 Fix

```rust
fn drop(&mut self, self_: Resource<UsbTransfer>) -> Result<(), Error> {
    trace!("Drop transfer");
    // await_transfer roept al table.delete aan; delete hier lukt dus alleen voor
    // transfers die nooit ge-await zijn.
    if let Ok(transfer) = self.table.delete(self_) {
        unsafe {
            if transfer.completed.load(Ordering::SeqCst) {
                // Callback heeft al libusb_free_transfer aangeroepen — niets doen.
            } else if transfer.receiver.is_some() {
                // Transfer is in-flight; de callback roept libusb_free_transfer aan.
                let _ = libusb_cancel_transfer(transfer.transfer);
            } else {
                // Nooit ingediend (new_transfer zonder submit_transfer).
                // Geen callback komt meer: zelf vrijgeven.
                libusb_free_transfer(transfer.transfer);
            }
        }
    }
    Ok(())
}
```

**Drie gevallen:**

| Toestand | `completed` | `receiver` | Actie |
|----------|-------------|------------|-------|
| Nooit ingediend | `false` | `None` | `libusb_free_transfer` |
| In-flight | `false` | `Some` | `libusb_cancel_transfer` (callback freed later) |
| Klaar, niet awaited | `true` | `None` | niets (callback freed al) |

---

## 5. Host overhead-logging (`instrument.rs`)

### 5.1 Doel

Het werkplan vereist "basislogging om overhead van de WebAssembly-sandbox te
analyseren (timing per USB-call, contextswitches, bufferformaten)".

### 5.2 Implementatie

Nieuw bestand `usb-wasi-host/src/instrument.rs`:

```rust
let _t = CallTrace::enter("submit_transfer")
    .detail(&format!("xfer_type={} len={} dir={}", ...));
// _t dropt bij einde scope → log-regel emitted
```

Elke `CallTrace` registreert:
- **`dur_us`** — wall-clock duur in microseconden
- **`ctx_vol_delta` / `ctx_nvol_delta`** — delta vrijwillige/onvrijwillige
  contextswitches via `/proc/self/status` (Linux only)
- **detail-string** — operatie-specifieke velden

### 5.3 Activeren

```bash
# Enkel overhead-trace tonen:
RUST_LOG=wasi_usb_trace=info usb-wasi-host -c component.wasm -- ...

# Trace + normale host-logs:
RUST_LOG=wasi_usb_trace=info,usb_wasi_host=info usb-wasi-host -c ...
```

### 5.4 Log-formaat

```
[INFO  wasi_usb_trace] op=submit_transfer dur_us=42 ctx_vol_delta=0 ctx_nvol_delta=1 xfer_type=Bulk len=65536 dir=In
[INFO  wasi_usb_trace] op=await_transfer  dur_us=18 ctx_vol_delta=1 ctx_nvol_delta=0
[INFO  wasi_usb_trace] op=new_transfer    dur_us=5  ctx_vol_delta=0 ctx_nvol_delta=0 xfer_type=Isochronous buf_size=32768 ep=0x81 iso_pkts=32
[INFO  wasi_usb_trace] op=claim_interface dur_us=12 ctx_vol_delta=0 ctx_nvol_delta=0 iface=1
[INFO  wasi_usb_trace] op=list_devices    dur_us=83 ctx_vol_delta=0 ctx_nvol_delta=0
[INFO  wasi_usb_trace] op=open_device     dur_us=29 ctx_vol_delta=0 ctx_nvol_delta=0
```

### 5.5 Geïnstrumenteerde methoden

| Methode | Bestand | detail-velden |
|---------|---------|---------------|
| `submit_transfer` | `main.rs` | `xfer_type`, `len`, `dir` |
| `await_transfer` | `main.rs` | — (duur = wachttijd op USB) |
| `new_transfer` | `main.rs` | `xfer_type`, `buf_size`, `ep`, `iso_pkts` |
| `list_devices` | `main.rs` | — |
| `list_devices_backend` | `usb_backend.rs` | — |
| `open_device` | `main.rs` | — |
| `claim_interface` | `usb_backend.rs` | `iface` |

### 5.6 Performance impact

Wanneer `RUST_LOG` het `wasi_usb_trace`-target niet bevat:
- `log::log_enabled!` is een snelle atomaire check (~1 ns)
- `Instant::now()` loopt altijd (~20 ns op moderne hardware), maar is
  verwaarloosbaar t.o.v. USB-transfer-latencies (≥10 µs)
- `/proc/self/status`-lezen (~10 µs) wordt overgeslagen als trace niet actief

---

## 6. W-bulk: SCSI WRITE(10) en random I/O

### 6.1 Motivatie

Het werkplan vereist: "*sequentiële reads/writes, random I/O, kleine en grote
transfers*". Alleen reads waren geïmplementeerd; dit breidt uit met schrijven
en random LBA-selectie.

### 6.2 Nieuwe CLI-argumenten

Beide `w_bulk.c` (C1/C2) en `w_bulk.rs` (C3/C4/C5) accepteren nu:

```
--mode read|write|rw    transfer mode (default: read)
--random                random LBA per iteratie (default: LBA 0)
```

| Mode | Beschrijving | Veiligheid |
|------|-------------|-----------|
| `read` | SCSI READ(10) (standaard, ongewijzigd) | ✅ veilig |
| `write` | SCSI WRITE(10), schrijft 0x5A-patroon | ⚠️ overschrijft LBA 0..N |
| `rw` | READ(10) dan WRITE(10) zelfde data terug | ✅ veilig (data unchanged) |

**Random I/O** (`--random`): selecteert willekeurig een start-LBA per iteratie
(uniform verdeeld, LCG in Rust, `rand()` in C). Stresseert de flash-controller
anders dan sequentiële reads.

### 6.3 Gewijzigde bestanden

| Bestand | Wijziging |
|---------|-----------|
| `usb-bench-c/src/w_bulk.c` | `scsi_write10()` functie; `bulk_mode_t` enum; `--mode`/`--random` parsing; `srand(time(NULL))`; `notes`-kolom in CSV |
| `usb-bench-rs/src/bin/w_bulk.rs` | `BulkMode` enum; `--mode`/`--random` parsing; LCG voor random LBA; `row.notes` met mode+lba |
| `usb-bench-rs/src/mass_storage.rs` | `write_blocks()` (SCSI WRITE 10); `readwrite_blocks()` (safe rw-pattern) |

### 6.4 CSV-output

De `notes`-kolom bevat nu per rij: `mode=read lba=0` (of `mode=write lba=2048`
etc.), zodat read- en write-iteraties achteraf te scheiden zijn in analyse.

---

## 7. Nieuwe documentatiebestanden

| Bestand | Inhoud |
|---------|--------|
| `WEBCAM-WASI.md` | Volledige documentatie van UVC-interactie via WASM: architectuur, probe/commit-protocol, bufferformaten, bandbreedte-berekening, latency-model, beperkingen macOS vs. Linux |
| `PLAN-TAAK7.md` | §5b: vergelijking met Warre Dujardin en Wouter Hennen — wat zij maten, wat wij herhalen, wat wij toevoegen, verwachte bulk-throughput vergelijking |

---

## 8. Volledig overzicht gewijzigde bestanden

| Bestand | Wijziging | Sectie |
|---------|-----------|--------|
| `libusb/libusb/os/wasi_usb.h` | `transfer_status` toegevoegd | §3 |
| `libusb/libusb/os/wasi_usb.c` | Deferred-completion fix | §3 |
| `libusb/libusb-wasi.a` | Herbouwd met patch | §3 |
| `libusb/libusb-wasi-rust.a` | Herbouwd met patch | §3 |
| `usb-wasi-host/src/instrument.rs` | **Nieuw** — `CallTrace` overhead-logging | §5 |
| `usb-wasi-host/src/main.rs` | `pub mod instrument`; `CallTrace` op 5 methoden; `drop`-fix; logs → `debug!` | §2, §4, §5 |
| `usb-wasi-host/src/usb_backend.rs` | `CallTrace` op `claim_interface`/`list_devices_backend`; logs → `debug!` | §2, §5 |
| `usb-bench-c/src/w_bulk.c` | `scsi_write10()`; `--mode`/`--random`; `notes`-kolom | §6 |
| `usb-bench-rs/src/bin/w_bulk.rs` | `BulkMode`; `--mode`/`--random`; LCG; `row.notes` | §6 |
| `usb-bench-rs/src/mass_storage.rs` | `write_blocks()`; `readwrite_blocks()` | §6 |
| `bench/run.sh` | Robuust; correcte VID:PIDs; relatieve CSV-paden; macOS unmount | §1 |
| `BENCHMARKING.md` | Hardware-tabel bijgewerkt | §1 |
| `WEBCAM-WASI.md` | **Nieuw** — webcam-architectuur en beperkingen | §7 |
| `PLAN-TAAK7.md` | **Nieuw** — volledig plan incl. §5b Warre/Wouter, §6 warmup | §7 |

> ⚠️ **Valkuil bij `ar r`:** dit commando matcht entries in een archive op
> **basename** van het inputbestand. Als je `ar r libusb-wasi.a wasi_usb_new.o`
> doet, wordt de entry `wasi_usb_new.o` toegevoegd, niet `wasi_usb.o` vervangen.
> Resultaat: oude én nieuwe object-bestanden zitten beide in de archive en de
> linker neemt willekeurig de oude. Correcte aanpak:
> ```bash
> cp wasi_usb_new.o wasi_usb.o     # rename naar de archive-entry-naam
> ar r libusb-wasi.a wasi_usb.o    # nu wordt de bestaande entry vervangen
> ```
> Verifieer met `ar t libusb-wasi.a | grep wasi_usb` (één regel verwacht).

---

## 6. Moet ik nog iets rebuilden?

**Nee, alles is al herbouwd.** Overzicht van de build-stappen die in deze
sessie al zijn uitgevoerd:

| Component | Commando | Status |
|-----------|----------|--------|
| `libusb-wasi.a` | `llvm-ar r … wasi_usb_new.o` | ✅ gedaan |
| `libusb-wasi-rust.a` | idem | ✅ gedaan |
| C2 WASM (`w_*.wasm`) | `cmake --build usb-bench-c/build-wasi` | ✅ gedaan |
| C4 WASM (`w_*.wasm`) | `bash bench/build-c4.sh` | ✅ gedaan |
| C5 WASM (`w_*.wasm`) | `cargo build --release --target wasm32-wasip2` | ✅ gedaan |
| `usb-wasi-host` | `cargo build --release` (in `usb-wasi-host/`) | ✅ gedaan |

C1 (native libusb C) en C3 (native rusb) zijn **niet** aangeraakt en hoeven
niet herbouwd te worden.

### Aanbevolen volgende stap

Voer een volledige benchmark-ronde uit met alle apparaten aangesloten:

```bash
just bench-run
```

Of voor een snelle smoke-test (1 iteratie per cel):

```bash
just bench-smoke
```

> **Opmerking iso/bulk op macOS:**
> - `bulk`: macOS koppelt de USB-stick automatisch aan. Het script probeert
>   `diskutil unmount` maar vindt het schijfknooppunt soms niet. Zo nodig
>   handmatig: `sudo diskutil unmount /dev/diskX`.
> - `iso`: macOS houdt de Logitech Brio 100-interface exclusief bezet. Elke
>   isochronische overdracht time-out na 5 seconden met 0 bytes. Voor echte
>   iso-data is Linux vereist (waar `libusb_detach_kernel_driver` werkt).

---

## 7. Technische context: C4 (rusb → libusb-wasi.a → WIT)

C4 is de Rust-benchmarkconditie waarbij `rusb` (via `libusb1-sys`) linkt
tegen `libusb-wasi-rust.a` in plaats van de native `libusb-1.0.so`. De
WIT-host handelt de USB-aanroepen af.

**Build-flow:**
```
usb-bench-rs/w_ctrl.rs
    └─ rusb 0.9  (Rust crate)
        └─ libusb1-sys 0.7  (FFI-laag)
            └─ libusb-wasi-rust.a  (WASI-backend van Robbe Leroy)
                └─ WIT-imports: component:usb/device@0.2.1, transfers@0.2.1
                    └─ usb-wasi-host  (Wasmtime + libusb1-sys native)
```

`bench/build-c4.sh` zet de pkg-config-omgeving zo dat
`libusb1-sys/build.rs` de `libusb-wasi-rust.a` vindt in plaats van de
systeem-libusb.
