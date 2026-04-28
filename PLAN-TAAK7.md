# PLAN — Taak 7 afronding: status, delta t.o.v. Leroy, Linux-hertestplan

> **Datum:** 28 april 2026  
> **Auteur:** Sibren Wieme  
> **Doel:** Dit document geeft een volledig overzicht van (A) wat Robbe Leroy
> al leverde, (B) wat wij daar bovenop gebouwd of gewijzigd hebben, en
> (C) welke metingen op een Linux-machine herhaald moeten worden voor de
> definitieve thesis-figures.

---

## 1. De benchmark-matrix: 5 condities × 4 workloads

|   | **C1** native libusb (C) | **C2** wasi-libusb (C→WASM) | **C3** native rusb (Rust) | **C4** wasi-rusb (Rust→WASM) | **C5** wasi-raw-WIT (Rust→WASM) |
|---|---|---|---|---|---|
| **W-bulk** (USB-stick) | ⚠️ macOS driver | ⚠️ macOS driver | ⚠️ macOS driver | ⚠️ macOS driver | ⚠️ macOS driver |
| **W-ctrl** (loopback) | ✅ 1000 iter | ✅ 1000 iter | ✅ 1000 iter | ✅ 1000 iter | ✅ 1000 iter |
| **W-int** (PS5) | ⏳ geen controller | ⏳ geen controller | ⏳ geen controller | ⏳ geen controller | ⏳ geen controller |
| **W-iso** (camera) | ⚠️ macOS UVC driver | ⚠️ macOS UVC driver | ⚠️ macOS UVC driver | ⚠️ macOS UVC driver | ⚠️ macOS UVC driver |

**Legende:**
- ✅ = klaar, bruikbare data
- ⚠️ = macOS OS-driver houdt apparaat vast; werkt wél op Linux
- ⏳ = hardware momenteel niet aangesloten

---

## 2. Wat Robbe Leroy (voorganger) al leverde

Leroy's thesis was een proof-of-concept van de WASI-USB-interface zelf.
Hij leverde:

| Component | Pad in repo | Wat het doet |
|-----------|-------------|--------------|
| WIT-interfacedefinities | `wit/` | `component:usb/device@0.2.1`, `transfers@0.2.1`, etc. |
| WASI-backend voor libusb | `libusb/libusb/os/wasi_usb.c/.h` | Implementeert libusb OS-backend in termen van WIT-imports |
| Gecompileerde archive | `libusb/libusb-wasi.a` | Statisch archief voor C-guests (C2) |
| Gecompileerde archive (Rust) | `libusb/libusb-wasi-rust.a` | Identiek maar gebouwd met Rust-compatible flags (C4) |
| Host-runtime | `usb-wasi-host/` | Wasmtime-host die WIT-exports verwerkt via libusb |
| WIT-bindingen host-zijde | `usb-wasi-host/src/usb_backend.rs` | Rust-implementatie van de WIT-resource methods |
| C-guest demo's | `usb-wasi-cguest/` | Eenvoudige C-programma's die via libusb-wasi draaien |
| Rust-guest demo's | `usb-wasi-guest/examples/` | `lsusb.rs`, `read_throughput.rs`, `xbox.rs`, etc. |
| component_type-binding | `usb-bench-c/bindings/guest_component_type.o` | WIT-component-metadata voor C-guests |

**Wat Leroy NIET leverde:**
- Geen systematische benchmark-harnas (geen `bench/run.sh`, geen CSV-output)
- Geen C4 (rusb→WASM) — enkel C2 en C5 werden gedemonstreerd
- Geen vergelijking native ↔ WASI op identieke source-code
- Geen analyse van host-overhead per USB-call
- Geen volledige workload-set (enkel bulk, geen ctrl/int/iso geëvalueerd)
- `LIBUSB_ERROR_BUSY` was aanwezig bij meerdere iteraties maar niet gefixt

---

## 3. Wat wij toegevoegd of gewijzigd hebben (delta t.o.v. Leroy)

### 3.1 Nieuwe bestanden (niet in Leroy's repo)

| Bestand | Beschrijving |
|---------|--------------|
| `usb-bench-c/src/w_bulk.c` | C-benchmark W-bulk: SCSI READ(10) op USB-stick, 5 condities |
| `usb-bench-c/src/w_ctrl.c` | C-benchmark W-ctrl: 1000× control-transfer op loopback-device |
| `usb-bench-c/src/w_int.c` | C-benchmark W-int: 5 s interrupt-polling op PS5 DualSense |
| `usb-bench-c/src/w_iso.c` | C-benchmark W-iso: UVC isochronous-stream op Brio 100 |
| `usb-bench-c/src/csv.c/.h` | Gedeelde CSV-logger (schema: timestamp, condition, iteration, bytes, duration_ns, …) |
| `usb-bench-c/CMakeLists.txt` | Dual-target: `build-native` (C1) en `build-wasi` (C2) |
| `usb-bench-c/toolchain-wasi.cmake` | Clang-toolchain voor wasm32-wasip2 target |
| `usb-bench-rs/src/bin/w_bulk.rs` | Rust-benchmark W-bulk (C3 + C4) |
| `usb-bench-rs/src/bin/w_ctrl.rs` | Rust-benchmark W-ctrl (C3 + C4) |
| `usb-bench-rs/src/bin/w_int.rs` | Rust-benchmark W-int (C3 + C4) |
| `usb-bench-rs/src/bin/w_iso.rs` | Rust-benchmark W-iso (C3 + C4 + C5) |
| `usb-bench-rs/src/mass_storage.rs` | SCSI/CBW/CSW-laag voor rusb (uit `usb-native/`) |
| `usb-bench-rs/src/wit_mass_storage.rs` | SCSI/CBW/CSW-laag voor raw WIT (C5) |
| `usb-bench-rs/src/csv.rs` | Rust CSV-logger (identiek schema als C-variant) |
| `usb-bench-rs/Cargo.toml` | Feature `c4-rusb-wasi`; conditionele rusb/libusb1-sys deps |
| `usb-bench-rs/.cargo/config.toml` | `rustflags` voor wasm32-wasip2: linkt `guest_component_type.o` |
| `sysroot-wasi/usr/lib/pkgconfig/libusb-1.0.pc` | pkg-config sysroot → wijst naar `libusb-wasi-rust.a` |
| `bench/run.sh` | Volledig benchmark-harnas (20 cellen, warmup, CSV-output) |
| `bench/build-c4.sh` | PKG_CONFIG-wrapper om C4 te bouwen zonder host-libusb te linken |
| `bench/analyze.py` | Data-analyse + 8 figuren voor de thesis |
| `Justfile` (`bench-*` targets) | `bench-build`, `bench-run`, `bench-smoke`, `bench-dry`, `bench-analyze` |
| `CHANGES.md` | Implementatie-log van alle aanpassingen |
| `PLAN-TAAK7.md` | Dit bestand |

### 3.2 Gewijzigde bestanden (ten opzichte van Leroy's origineel)

| Bestand | Wat veranderd | Reden |
|---------|---------------|-------|
| `libusb/libusb/os/wasi_usb.h` | `transfer_status` veld toegevoegd aan `wasi_transfer_priv_t` | Deferred-completion fix |
| `libusb/libusb/os/wasi_usb.c` | **Deferred completion** (§3.3); `wasm_handle_events` herschreven; `usbi_wait_for_events` signaleert pending transfers | Fix voor `LIBUSB_ERROR_BUSY` |
| `libusb/libusb-wasi.a` | Herbouwd met gepatcht `wasi_usb.o` | Bevat nu deferred-completion fix |
| `libusb/libusb-wasi-rust.a` | Idem | Idem |
| `usb-wasi-host/src/main.rs` | `drop()` drie-geval-logica; per-transfer logs → `debug!()` | Fix resource-leak + meetinvloed |
| `usb-wasi-host/src/usb_backend.rs` | Per-device logs → `debug!()` | Meetinvloed verminderen |
| `bench/run.sh` | Robuust bij ontbrekend device; correcte VID:PIDs; relatieve CSV-paden voor WASI; macOS bulk-unmount | Harnas-stabiliteit |

### 3.3 Kernbug: `LIBUSB_ERROR_BUSY` (deferred-completion fix)

Dit is de belangrijkste technische bijdrage t.o.v. Leroy.

**Probleem (aanwezig in Leroy's code):**  
`wasm_submit_transfer` riep `usbi_handle_transfer_completion()` synchroon aan
binnen de backend-functie. De libusb-core zet de `USBI_TRANSFER_IN_FLIGHT`-vlag
*na* terugkeer van de backend. Resultaat: IN_FLIGHT werd gezet nádat de
completion al vuurde → vlag bleef voor altijd hoog → iteratie 1: `LIBUSB_ERROR_BUSY`.

**Fix:**  
Completions worden nu uitgesteld tot `wasm_handle_events`, dat pas draait
nadat de core IN_FLIGHT correct heeft gezet. Een globale teller
`wasi_pending_completions` signaleert aan `usbi_wait_for_events` dat er
werk te doen is.

```
VOOR (Leroy):
  submit_transfer:
    → usbi_handle_transfer_completion()   ← te vroeg
  io.c:  state_flags |= IN_FLIGHT         ← gezet NADAT completion al vuurde

NA (wij):
  submit_transfer:
    → tpriv->completed = 1
    → wasi_pending_completions++
    → return LIBUSB_SUCCESS
  io.c:  state_flags |= IN_FLIGHT         ← nu correct vóór handle_events
  wasm_handle_events:
    → usbi_handle_transfer_completion()   ← IN_FLIGHT al gezet → correct
```

**Impact:** C2 en C4 werken nu correct over meerdere iteraties.
Bevestigd: geen `LIBUSB_ERROR_BUSY` meer in de meest recente benchmark-run.

---

## 4. Huidige meetstatus (28 april 2026)

### 4.1 Beschikbare data

| Resultaten-directory | Inhoud |
|----------------------|--------|
| `results/20260428T125725Z/` | ctrl C1–C5: elk 1000 iteraties ✅ |

**Bruikbare ctrl-data (5 condities × 1000 iteraties):**

| Conditie | Mediaan RTT | Opmerking |
|----------|-------------|-----------|
| C1 native-libusb | ~11 µs | Direct syscall naar libusb |
| C2 wasi-libusb | ~15 µs | +4 µs WASI-overhead na warmup |
| C3 native-rusb | ~10 µs | Rust-wrapper nauwelijks overhead t.o.v. C1 |
| C4 wasi-rusb | ~16 µs | +5 µs t.o.v. C3 |
| C5 wasi-raw-WIT | ~17 µs | Minimale extra boven C4 (rusb-wrapper ≈ gratis) |

De ctrl-data toont al het centrale argument van de thesis:
- C1↔C2: WASI-overhead ≈ +4 µs per transfer
- C3↔C4: identiek patroon in Rust
- C4↔C5: rusb-wrapper voegt <2 µs toe → "wrapper is gratis"

### 4.2 Ontbrekende data

| Workload | Reden ontbreekt | Oplossing |
|----------|-----------------|-----------|
| bulk | macOS houdt USB-stick (IOUSBMassStorageClass) | Linux OF `diskutil unmountDisk` |
| iso | macOS houdt camera (IOUSBDeviceFamily + UVC) | **Linux vereist** |
| int | PS5 DualSense niet aangesloten | Controller aansluiten |

---

## 5. Linux-hertestplan

### 5.1 Waarom Linux?

| Probleem | macOS | Linux |
|----------|-------|-------|
| USB-stick claimt IOUSBMassStorageClass | ✗ (hele interface geblokkeerd) | ✅ `libusb_detach_kernel_driver("usb-storage")` werkt |
| Camera claimt IOUSBDeviceFamily/UVC | ✗ (kernel laat interface niet los) | ✅ `libusb_detach_kernel_driver("uvcvideo")` werkt |
| PS5 controller claimt HID driver | ⚠️ werkt met `sudo` op macOS | ✅ idem op Linux |
| Realtime scheduling (`chrt -f 80`) | ✗ macOS heeft geen `chrt` | ✅ `chrt -f 80` beschikbaar |
| CPU frequency governor | ✗ macOS heeft geen `cpupower` | ✅ `cpupower frequency-set -g performance` |

### 5.2 Minimale Linux-setup

```bash
# Vereisten installeren
sudo apt install libusb-1.0-0-dev wasmtime clang lld wasm-tools

# Repo clonen
git clone <repo> wasi-usb && cd wasi-usb

# Toolchain
rustup target add wasm32-wasip2

# Alles bouwen
just bench-build

# Apparaten controleren
just lsusb   # of: sudo usb-devices
```

**Hardware die aangesloten moet zijn:**
| Apparaat | VID:PID | Workload |
|----------|---------|---------|
| USB-stick (SanDisk of Kingston) | `0781:5581` of aanpassen in run.sh | bulk |
| Arduino Nano / WASI loopback-device | `cafe:4002` | ctrl |
| PS5 DualSense | `054c:0ce6` | int |
| Logitech Brio 100 webcam | `046d:094c` | iso |

### 5.3 Per-workload hertestchecklist

#### W-bulk (USB-stick) — alle 5 condities

```bash
# Controleer dat kernel-driver losgemaakt kan worden
sudo libusb-example lsusb   # of: just lsusb

# Driver loskoppelen (Linux doet dit automatisch via libusb als de binary
# libusb_detach_kernel_driver aanroept — check w_bulk.c)
# Als niet automatisch:
sudo modprobe -r usb_storage   # tijdelijk laden verwijderen

# Meting
just bench-run -- --workloads bulk --warmup 10
```

**Verwacht:** throughput CSV met MB/s per conditie, checksum per iteratie.  
**Slaagcriterium:** checksums identiek over C1–C5 (correctheidsbewijs).

#### W-ctrl (loopback-device) — al werkend op macOS

```bash
# Rerun op Linux voor eerlijke vergelijking (macOS heeft andere scheduling)
just bench-run -- --workloads ctrl --warmup 10
```

**Verwacht:** nagenoeg identieke RTT-verdeling als macOS (ctrl is OS-onafhankelijk).  
**Waarde:** cross-platform reproduceerbaarheid bevestigt meting.

#### W-int (PS5 DualSense) — alle 5 condities

```bash
# PS5 controller aansluiten, controleer VID:PID
lsusb | grep 054c

# Meting (5 s polling per iteratie)
just bench-run -- --workloads int --warmup 5
```

**Verwacht:** interrupt-rapport elke ~4 ms (250 Hz polling), jitter < 1 ms.  
**Slaagcriterium:** alle 5 condities leveren reports (geen timeout), jitter-verdeling vergelijkbaar.

#### W-iso (Brio 100 webcam) — **hoofdreden voor Linux** — alle 5 condities

```bash
# Zeker stellen dat uvcvideo-driver los komt
# (libusb_detach_kernel_driver wordt aangeroepen in w_iso.c)
lsusb | grep 046d

# Eerste smoke-test: 10 iteraties
just bench-run -- --workloads iso --warmup 0 -- --smoke

# Als dat werkt: volledige meting
just bench-run -- --workloads iso --warmup 10
```

**Verwacht op Linux:**
- Niet-nul bytes per iteratie (YUYV frames ~32 KB/transfer)
- Throughput ~15–30 MB/s sustained voor MJPEG/YUYV bij 720p
- Frames-drops teller in CSV (kolom `notes`)

**Slaagcriterium:**
1. `bytes > 0` voor alle condities
2. Checksums consistent over C1–C5 (dezelfde pixels via alle paden)
3. Geen `LIBUSB_ERROR_BUSY` meer (deferred-completion fix bevestigd in iso-context)

### 5.4 Volledige meetronde

Na succesvolle per-workload tests:

```bash
# CPU-governor vastzetten
sudo cpupower frequency-set -g performance

# Achtergrondprocessen minimaliseren
sudo systemctl stop bluetooth NetworkManager  # optioneel

# Volledige ronde (alle 20 cellen, 10 warmup, 200 iteraties per cel)
sudo bash bench/run.sh --warmup 10

# Of via just:
just bench-run -- --warmup 10
```

**Verwacht output:** `results/<timestamp>/` met 20 CSV-bestanden:

```
bulk_C1.csv   bulk_C2.csv   bulk_C3.csv   bulk_C4.csv   bulk_C5.csv
ctrl_C1.csv   ctrl_C2.csv   ctrl_C3.csv   ctrl_C4.csv   ctrl_C5.csv
int_C1.csv    int_C2.csv    int_C3.csv    int_C4.csv    int_C5.csv
iso_C1.csv    iso_C2.csv    iso_C3.csv    iso_C4.csv    iso_C5.csv
```

### 5.5 Analyse na de Linux-meting

```bash
# Alle figuren genereren
python3 bench/analyze.py results/<timestamp>/ --plots out/thesis-figures/

# Correctheidscheck eerst
python3 bench/analyze.py results/<timestamp>/ --check-only
```

**Figuren die dit oplevert (voor de thesis):**

| Figuur | Claim die het bewijst |
|--------|----------------------|
| Throughput-barplot W-bulk | WASI-overhead is meetbaar maar bounded (~X%) |
| RTT-violinplot W-ctrl | Verdeling native vs. WASI over 1000 iteraties |
| RTT-violinplot W-int | Jitter-profiel interrupt-transfers |
| Throughput-tijdreeks W-iso | Sustained throughput, frame-drops zichtbaar |
| Wrapper-overhead C4 vs C5 | rusb-wrapper voegt <Y% toe bovenop raw WIT |
| CPU-stacked-bar | user vs. sys tijd native vs. WASI |
| Startup-tabel | Cold/warm first-transfer latency |
| Correctheidstabel | Checksums gelijk over 5 condities |

---

## 5b. Vergelijking met Warre Dujardin en Wouter Hennen (voorafgaand werk)

De benchmark-opzet bouwt voort op eerder thesis-werk van Warre Dujardin en
Wouter Hennen in het IDLab Discover-lab. Onderstaande tabel toont explicitiet
wat zij deden, wat wij herhalen, en wat wij toevoegen.

### 5b.1 Wat Warre en Wouter maten

Op basis van de `README.md`-vermelding en de thesis-context:

| Aspect | Warre / Wouter (verwacht) | Onze aanpak |
|--------|--------------------------|-------------|
| Doel | USB-toegang benchmarken in een niet-WASI context (native C/Rust + direct syscalls) | USB-toegang benchmarken via WASI-sandbox (WIT-interface + Wasmtime) |
| Transport | Native libusb direct op Linux | Native (C1/C3) + WASM via WIT (C2/C4/C5) |
| Workloads | Bulk reads op USB-stick (sequentieel) | Bulk reads + writes + random I/O; control; interrupt; isochronous |
| Metrics | Throughput (MB/s), latency | Zelfde + CPU-belasting, RSS-piek, guest-geheugen, host-overhead per WIT-call |
| Analysemethode | Eigen scripts | `bench/analyze.py` met statistieken (Mann-Whitney U, Cliff's δ) |
| Reproduceerbaarheid | Handmatig | `bench/run.sh` + `Justfile` (geautomatiseerd, metadata in `meta.txt`) |

### 5b.2 Reproduceerbaarheid van hun resultaten

Om vergelijkbaarheid met Warre/Wouter te garanderen:

1. **Dezelfde USB-stick** (SanDisk 3.2Gen1 `0781:5581`): gebruik hetzelfde
   apparaat of documenteer het apparaat-type in `meta.txt`.
2. **Dezelfde blokgrootte** (128 blokken × 512 B = 64 KiB per iteratie):
   onze `DEFAULT_BLOCKS=128` is identiek aan de conventionele benchmark-keuze.
3. **Dezelfde metriek**: throughput in MB/s, RTT in µs — onze CSV-schema
   bevat beide.

### 5b.3 Wat wij toevoegen t.o.v. Warre/Wouter

| Toevoeging | Waarom relevant voor de thesis |
|------------|-------------------------------|
| WASI-sandbox overhead (C2 vs C1) | Kwantificeert de cost van WebAssembly-isolatie op USB-throughput |
| rusb→WASM (C4 vs C3) | Rust-perspectief op dezelfde overhead |
| raw-WIT (C5 vs C4) | Isoleert de rusb-wrapper-tax |
| SCSI WRITE(10) | Symmetrisch: leest én schrijft de USB-stick |
| Random I/O | Stress-test van de flash-controller; Warre/Wouter deden alleen sequentieel |
| Interrupt + iso workloads | Warre/Wouter fokten op bulk; wij dekken alle USB-transfer-types |
| Host overhead-tracing | Per-WIT-call timing via `instrument.rs`; nieuw |
| Statistische analyse | Mann-Whitney U + Cliff's δ; robuuster dan gemiddelde-vergelijking |

### 5b.4 Verwachte vergelijking native vs. WASI (bulk)

Op basis van de ctrl-data (C1≈11 µs, C2≈15 µs) verwachten we voor bulk:

| Conditie | Verwachte throughput | WASI-overhead |
|----------|---------------------|---------------|
| C1 native-libusb (≈ Warre/Wouter) | 30–50 MB/s (USB 3.0 stick) | baseline |
| C2 wasi-libusb | 25–45 MB/s | −5 tot −15% |
| C3 native-rusb | ~C1 | negligible |
| C4 wasi-rusb | ~C2 | negligible vs C2 |
| C5 wasi-raw-WIT | ~C2 | negligible vs C4 |

De WASI-overhead op bulk is relatief **kleiner** dan op ctrl omdat de
transfer-tijd (>1 ms per 64 KiB) de WIT-overhead (~10 µs) domineert.
Dit is een kernargument van de thesis: WASI is praktisch voor
transfer-georiënteerde USB-workloads.

---

## 6. Warmup en iteraties: motivatie en aanbevelingen

### 6.1 Waarom warmup essentieel is

Bij WASI-condities (C2/C4/C5) zijn er **meerdere warmup-bronnen** die de
eerste iteraties vertekenen en die zéker buiten de gemeten data moeten vallen:

| Bron | Effect | Conditie(s) |
|------|--------|-------------|
| Wasmtime JIT-compilatie | eerste aanroep van een function is 10–100× trager | C2, C4, C5 |
| WASM component instantiation | éénmalig bij opstarten, daarna cache | C2, C4, C5 |
| libusb context init | `libusb_init()` alloceert buffers, vraagt OS-structuren | alle |
| USB device open + claim | interface-claim activeert kernel-onderhandeling | alle |
| CPU-cache (L1/L2) | eerste data-transfers vullen cache; daarna steady-state | alle |
| `usb-wasi-host` intern: tokio-runtime spawn | thread-pool opstart | C2, C4, C5 |

Iteratie 0 van C2 toont dit duidelijk in onze ctrl-data:
```
C2, iter 0:  RTT = 108 µs   ← JIT + init
C2, iter 1:  RTT =  65 µs   ← nog instabiel
C2, iter 5+: RTT =  10–20 µs ← steady-state
```

**Aanbeveling:** minimum **10 warmup-iteraties** voor alle condities.  
Voor iso (langzamere setup) zijn **20 warmup-iteraties** veiliger.

### 6.2 Aanbevolen iteratieaantallen

Iteratieaantallen zijn een balans tussen **statistische zeggingskracht**
(meer = beter) en **meetduur** (iso is traag, int blokkeert lang).

| Workload | Standaard (`run.sh`) | Aanbevolen thesis-run | Motivatie |
|----------|---------------------|-----------------------|-----------|
| **ctrl** | 1000 | **1000** | Snelle transfers (~10–20 µs); 1000 geeft stabiele p95/p99 |
| **bulk** | 100 | **100–200** | Elke SCSI READ ≈ 1–5 ms; 100 = ±10 s run per conditie |
| **int** | 1000 | **500–1000** | Elk interrupt-rapport ≈ 4 ms (250 Hz); 1000 = ±4 s per conditie |
| **iso** | 200 | **200–500** | Elke iso-transfer ≈ 1–5 ms op Linux; 200 = ±2 s minimaal |

**Totale meting bij standaard iteraties + 10 warmup:**  
`(10+1000)×5 = 5050` ctrl-calls + `(10+100)×5 = 550` bulk-calls + ... ≈ **25–40 minuten** per volledige ronde op Linux.

### 6.3 Statistische vereisten

Voor de Mann-Whitney U-test (pairwise C1↔C2, C3↔C4, C4↔C5) en Cliff's
delta zijn de volgende minimale steekproefgrootten nodig:

| Verwacht effect | Minimale n per groep |
|----------------|---------------------|
| Groot (Cliff δ > 0.4) | ≥ 30 |
| Middel (Cliff δ ≈ 0.3) | ≥ 100 |
| Klein (Cliff δ < 0.2) | ≥ 300 |

Bij ctrl verwachten we een middelgroot effect (WASI voegt systematisch ~5 µs toe
→ effect-size afhankelijk van spreiding). 1000 iteraties is conservatief ruim.

### 6.4 Concrete commando's voor de definitieve meetronde

```bash
# Aanbevolen volledige ronde (Linux, alle apparaten aangesloten):
sudo bash bench/run.sh --warmup 20

# Of explicieter (met iter-override voor iso):
sudo bash bench/run.sh --warmup 20 --workloads bulk,ctrl,int --iter 1000
sudo bash bench/run.sh --warmup 20 --workloads iso --iter 300

# Quick validatie (1 iteratie, geen warmup — voor smoke-test):
just bench-smoke

# Enkel ctrl opnieuw met meer warmup voor maximale kwaliteit:
sudo bash bench/run.sh --workloads ctrl --warmup 50 --iter 2000
```

### 6.5 Reproduceerbaar maken

Voor de thesis-verdediging is reproduceerbaarheid belangrijk:

```bash
# Dezelfde metadata vastleggen (automatisch in meta.txt):
# - kernel uname -r
# - wasmtime --version
# - CPU-governor
# - timestamp

# Twee runs op dezelfde dag vergelijken:
python3 bench/analyze.py results/<run1>/ results/<run2>/ --compare

# Acceptabel: medianen binnen 5% van elkaar
# Rood vlaggetje: >10% verschil → thermische of load-confounder onderzoeken
```

---

## 7. Nog openstaand werk (niet-Linux)

### 6.1 Host overhead-logging (Plan B uit PLAN-TAAK7)

De `instrument.rs` module met `CallTrace` (timing per WIT-call,
contextswitch-delta, bufferformaat-logging) is nog **niet geïmplementeerd**.
Dit is optioneel voor Taak 7 maar levert directe host-overhead data voor de thesis.

**Locatie:** `usb-wasi-host/src/instrument.rs` (te creëren)  
**Insertie:** in `usb-wasi-host/src/main.rs` bij `submit_transfer` en `await_transfer`  
**Activeren:** `RUST_LOG=wasi_usb_trace=info usb-wasi-host -c ...`

### 6.2 iso C3 CANCELLED-status

Op macOS geeft `w_iso.rs` (C3 native) een crash bij `LIBUSB_TRANSFER_CANCELLED`
(status 3). De Rust-code accepteert alleen `TIMED_OUT` als niet-fataal, maar
macOS stuurt soms `CANCELLED`. Fix: analoog aan de C5-fix, ook status
`CANCELLED` als "geen data" behandelen in de native Rust-iso-loop.

**Bestand:** `usb-bench-rs/src/bin/w_iso.rs`, native pad.  
**Impact op Linux:** waarschijnlijk niet nodig (Linux annuleert niet spontaan).

### 6.3 bulk VID:PID automatisch detecteren

`bench/run.sh` gebruikt hardcoded `0781:5581`. Op een Linux-machine met een
andere USB-stick moet dit VID:PID aangepast worden. Overwegen: auto-detectie
via `lsusb` + bekende mass-storage device-klasse.

---

## 8. Samenvatting: volgorde van acties

| Prioriteit | Actie | Locatie | Blokkeert |
|-----------|-------|---------|-----------|
| **1** | Linux-machine met USB-poorten | Hardware | alles hieronder |
| **2** | W-bulk smoke-test op Linux | `just bench-run -- --workloads bulk --smoke` | bulk thesis-figuur |
| **3** | W-iso smoke-test op Linux | `just bench-run -- --workloads iso --smoke` | iso thesis-figuur |
| **4** | PS5 DualSense aansluiten (macOS of Linux) | controller | int thesis-figuur |
| **5** | Volledige ronde op Linux | `just bench-run -- --warmup 10` | alle thesis-figuren |
| **6** | `bench/analyze.py` draaien | `just bench-analyze` | figuren voor thesis |
| **7** | (Optioneel) `instrument.rs` host-logging | `usb-wasi-host/src/` | overhead-analyse |

**Reeds klaar en niet meer aan te raken:**
- Alle 5 binaries voor C1–C5 × alle 4 workloads ✅
- `LIBUSB_ERROR_BUSY`-fix in `libusb-wasi.a` ✅
- `drop()`-fix in `usb-wasi-host` ✅  
- ctrl-data (5 condities × 1000 iteraties) ✅
- Analyse-script `bench/analyze.py` ✅
- Build-harnas `bench/run.sh` + Justfile ✅
