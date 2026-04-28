# WASI-USB Benchmark Suite

Uitgebreide documentatie van de benchmark-suite voor de masterthesis
*"WASI-USB: een WebAssembly System Interface voor USB-hardware"* (Sibren Wieme, 2026).

---

## Inhoudsopgave

1. [Benchmark-matrix](#1-benchmark-matrix)
2. [Hardwarevereisten](#2-hardwarevereisten)
3. [Softwarevereisten](#3-softwarevereisten)
4. [Repository-structuur](#4-repository-structuur)
5. [Bouwen](#5-bouwen)
   - [5.1 Alles in één commando](#51-alles-in-één-commando)
   - [5.2 Per conditie](#52-per-conditie)
   - [5.3 C4-specifieke setup (libusb-wasi-rust.a)](#53-c4-specifieke-setup-libusb-wasi-rusta)
6. [Uitvoeren](#6-uitvoeren)
7. [Resultaten analyseren](#7-resultaten-analyseren)
8. [CSV-schema](#8-csv-schema)
9. [Technische diepte: C4-implementatie](#9-technische-diepte-c4-implementatie)
10. [Troubleshooting](#10-troubleshooting)

---

## 1. Benchmark-matrix

De evaluatie vergelijkt USB-toegang in **vijf condities** over **vier workloads**,
totaal 20 meetcellen.

### Condities

| ID | Naam           | Taal  | Runtime        | Beschrijving |
|----|----------------|-------|----------------|--------------|
| C1 | native-libusb  | C     | OS-native ELF  | Directe libusb-aanroepen, geen WASI |
| C2 | wasi-libusb    | C     | wasmtime + WIT | C-code gecompileerd voor WASM, libusb-wasi backend |
| C3 | native-rusb    | Rust  | OS-native ELF  | rusb-aanroepen via libusb, geen WASI |
| C4 | wasi-rusb      | Rust  | wasmtime + WIT | **rusb → libusb-wasi.a → WIT** (zelfde Rust-broncode als C3) |
| C5 | wasi-raw-wit   | Rust  | wasmtime + WIT | Directe `component:usb::*`-aanroepen via wit-bindgen |

**Centrale claims die de matrix bewijst:**
- **C1 ↔ C2**: totale WASI-overhead voor C (runtime + WIT-grenslaag)
- **C3 ↔ C4**: totale WASI-overhead voor Rust (idem)
- **C4 ↔ C5**: pure rusb-wrapper-belasting bovenop directe WIT-aanroepen
- **C1 ≈ C3** en **C2 ≈ C4**: taal is geen confounder; bottleneck zit in de WIT-grenslaag

### Workloads

| ID   | Transfer       | Apparaat                        | VID:PID   | Beschrijving |
|------|----------------|---------------------------------|-----------|--------------|
| bulk | Bulk           | SanDisk 3.2Gen1 USB-stick       | 0781:5581 | 30× SCSI READ(10), 512 KB per transfer |
| ctrl | Control        | WASI-USB Loopback device        | cafe:4002 | 1000× control transfers (RTT-verdeling) |
| int  | Interrupt      | PS5 DualSense controller        | 054c:0ce6 | 1000× interrupt-IN poll (jitter) |
| iso  | Isochronous    | Logitech Brio 100 webcam        | 046d:094c | 200× UVC YUYV-frame (doorvoer) |

---

## 2. Hardwarevereisten

Alle vier USB-apparaten zijn nodig voor een volledige meetronde.
Afzonderlijke werkloads kunnen ook apart worden gedraaid.

| Apparaat | Aansluiting | Opmerkingen |
|---|---|---|
| SanDisk 3.2Gen1 (of gelijkwaardige USB 3.x stick) | USB-A / USB-C | Ontkoppel OS-mountpoint vóór de test |
| WASI-USB Loopback device (`cafe:4002`) | USB | Raspberry Pi Pico of gelijkwaardige loopback firmware |
| PS5 DualSense (of DualShock 4) | USB-C | Losgekoppeld van draadloze modus |
| Logitech Brio 100 (of gelijkwaardige UVC-webcam) | USB-A | Sluit geen andere videosoftware |

---

## 3. Softwarevereisten

### Vereist

| Tool | Versie | Waarvoor |
|---|---|---|
| Rust + rustup | ≥ 1.80 | C3, C4, C5 bouwen |
| `wasm32-wasip2` target | via `rustup target add wasm32-wasip2` | C4, C5 |
| WASI SDK | ≥ 24 (standaard `/opt/wasi-sdk`) | C2 bouwen, libusb-wasi.a patchen |
| CMake | ≥ 3.20 | C1, C2 bouwen |
| pkg-config | elke versie | C4 build.rs |
| wasm-tools | ≥ 1.0 | verificatie en analyse |
| Python 3 | ≥ 3.9 | analyse-script |

### Optioneel

| Tool | Waarvoor |
|---|---|
| `just` | recepten uit `Justfile` |
| `pandas`, `seaborn`, `scipy` | `bench/analyze.py` grafieken |

---

## 4. Repository-structuur

```
wasi-usb/
├── wit/                          # WIT-bronbestanden (component:usb@0.2.1)
├── usb-wasi-host/                # wasmtime-gebaseerde host-runtime
├── usb-bench-c/                  # C-benchmarks (C1 + C2)
│   ├── src/
│   │   ├── csv.{c,h}             # gedeelde CSV-logger
│   │   ├── w_bulk.c              # W-bulk benchmark
│   │   ├── w_ctrl.c              # W-ctrl benchmark
│   │   ├── w_int.c               # W-int benchmark
│   │   └── w_iso.c               # W-iso benchmark
│   ├── bindings/
│   │   ├── guest.{h,c}           # WIT C-bindingen (gegenereerd door wit-bindgen)
│   │   └── guest_component_type.o # component-type sectie voor wasm-component-ld
│   ├── CMakeLists.txt            # dual-target build (C1 + C2)
│   └── toolchain-wasi.cmake      # wasm32-wasip2 toolchain-bestand
├── usb-bench-rs/                 # Rust-benchmarks (C3 + C4 + C5)
│   ├── src/
│   │   ├── lib.rs                # gedeelde modules
│   │   ├── csv.rs                # gedeelde CSV-logger
│   │   ├── bindings.rs           # wit-bindgen macros (C5)
│   │   ├── bulk_only.rs          # mass-storage transportlaag
│   │   └── bin/
│   │       ├── w_bulk.rs         # W-bulk benchmark
│   │       ├── w_ctrl.rs         # W-ctrl benchmark
│   │       ├── w_int.rs          # W-int benchmark
│   │       └── w_iso.rs          # W-iso benchmark
│   ├── build.rs                  # geeft cguest_component_type.o mee als linker arg (C4)
│   ├── Cargo.toml                # bevat c4-rusb-wasi feature
│   └── target-wasi-rusb/         # C4 build-output (apart van C5)
├── bench/
│   ├── build-c4.sh               # bouwt C4-binaries (PKG_CONFIG + cargo)
│   ├── run.sh                    # benchmark-harness (alle 5 condities)
│   └── analyze.py                # data-analyse en grafieken
├── libusb/
│   ├── libusb-wasi.a             # Robbe Leroy's WIT-backed libusb (C2 + C4)
│   └── libusb-wasi-rust.a        # gepatchte variant voor Rust-linker (C4)
├── sysroot-wasi/                 # WASI pkg-config sysroot (C4)
│   └── usr/lib/
│       ├── pkgconfig/
│       │   └── libusb-1.0.pc     # wijst naar libusb-wasi-rust.a
│       └── cguest_component_type.o
├── Justfile                      # just-recepten
└── BENCHMARKING.md               # dit bestand
```

---

## 5. Bouwen

### 5.1 Alles in één commando

```bash
just bench-build
```

Dit voert achtereenvolgens uit:
1. CMake native build → C1-binaries (`usb-bench-c/build-native/`)
2. CMake WASI build → C2-binaries (`usb-bench-c/build-wasi/`)
3. `cargo build --release` → C3-binaries (`usb-bench-rs/target/release/`)
4. `bash bench/build-c4.sh` → C4-binaries (`usb-bench-rs/target-wasi-rusb/wasm32-wasip2/release/`)
5. `cargo build --target wasm32-wasip2` → C5-binaries (`usb-bench-rs/target/wasm32-wasip2/release/`)

Bouw ook de host-runtime als die nog niet gebouwd is:
```bash
just build-host
```

### 5.2 Per conditie

```bash
# C1 — native libusb (C)
cmake -B usb-bench-c/build-native usb-bench-c
cmake --build usb-bench-c/build-native

# C2 — wasi-libusb (C WASM)
cmake -B usb-bench-c/build-wasi usb-bench-c \
    -DCMAKE_TOOLCHAIN_FILE=usb-bench-c/toolchain-wasi.cmake
cmake --build usb-bench-c/build-wasi

# C3 — native rusb (Rust)
cargo build --release --bins --manifest-path usb-bench-rs/Cargo.toml

# C4 — wasi-rusb (Rust + rusb → libusb-wasi.a → WIT)
bash bench/build-c4.sh

# C5 — wasi-raw-wit (Rust WASM)
cargo build --release --bins --target wasm32-wasip2 \
    --manifest-path usb-bench-rs/Cargo.toml
```

### 5.3 C4-specifieke setup (`libusb-wasi-rust.a`)

**Eenmalige stap vóór de eerste C4-build.**

`libusb-wasi.a` (Robbe Leroy's WIT-backed libusb) bevat `cguest.o` — een
C-gegenereerd WIT-bindingsobject. Dat object werkt goed voor C-binaries (C2),
maar is incompatibel met Rust's `wasm-component-ld` door een C-specifieke
run-export handler. Zie [sectie 9](#9-technische-diepte-c4-implementatie) voor
de volledige technische uitleg.

De fix: maak `libusb-wasi-rust.a` — hetzelfde archief maar met `cguest.o`
waarvan de run-export handler als interne (niet-geëxporteerde) functie is
gemarkeerd, zodat `--gc-sections` hem verwijdert.

```bash
# Stap 1: Pak alle .o-bestanden uit het originele archief
WASI_SDK=/opt/wasi-sdk
REPO=/Users/sibrenwieme/Documents/Masterproef/wasi-usb

mkdir -p /tmp/repack_ar
cd /tmp/repack_ar
$WASI_SDK/bin/llvm-ar xv $REPO/libusb/libusb-wasi.a

# Stap 2: Patch de run-export-vlaggen in cguest.o
#
# Het symbool __wasm_export_exports_wasi_cli_run_run heeft flags
# EXPORTED|NO_STRIP (bytes [a4 01] op offset 0x9150 in dit specifieke .o).
# We veranderen die naar VISIBILITY_HIDDEN (bytes [84 00]):
cp cguest.o /tmp/cguest_modified.o
printf '\x84' | dd of=/tmp/cguest_modified.o bs=1 seek=$((0x9150)) conv=notrunc
printf '\x00' | dd of=/tmp/cguest_modified.o bs=1 seek=$((0x9151)) conv=notrunc

# Controleer resultaat (verwacht: VISIBILITY_HIDDEN, GEEN EXPORTED of NO_STRIP)
wasm-tools dump /tmp/cguest_modified.o | grep "__wasm_export_exports_wasi_cli"
# → Func { flags: SymbolFlags(VISIBILITY_HIDDEN), ... }

# Stap 3: Maak het nieuwe archief (zonder run-export-handler)
$WASI_SDK/bin/llvm-ar rcs $REPO/libusb/libusb-wasi-rust.a \
    /tmp/repack_ar/core.o \
    /tmp/repack_ar/descriptor.o \
    /tmp/repack_ar/hotplug.o \
    /tmp/repack_ar/io.o \
    /tmp/repack_ar/strerror.o \
    /tmp/repack_ar/sync.o \
    /tmp/repack_ar/wasi_usb.o \
    /tmp/cguest_modified.o
```

> **Let op:** de byte-offset `0x9150` is specifiek voor de huidige versie van
> `libusb-wasi.a` (MD5: `cb106e351040c50232c809624fe08845`). Als het archief
> herbouwd wordt, moet je de offset opnieuw opzoeken via:
> ```bash
> wasm-tools dump libusb/libusb-wasi.a | grep -A1 "__wasm_export_exports_wasi_cli_run_run"
> ```
> en de twee vlagbytes aanpassen.

**Verificatie:**
```bash
# pkg-config moet -lusb-wasi-rust teruggeven (NIET -lusb-1.0 of -lusb-wasi)
PKG_CONFIG_LIBDIR=$REPO/sysroot-wasi/usr/lib/pkgconfig \
PKG_CONFIG_ALLOW_CROSS=1 \
pkg-config --libs libusb-1.0
# Verwacht: -L.../libusb -lusb-wasi-rust

# C4-binaries bevatten WIT-imports
wasm-tools print usb-bench-rs/target-wasi-rusb/wasm32-wasip2/release/w_bulk.wasm \
    | grep "component:usb" | head -5
# Verwacht: (import "component:usb/device@0.2.1" ...)
```

---

## 6. Uitvoeren

### Snelle sanity-check (smoke)

```bash
# 1 iteratie per cel, alle condities en workloads
just bench-smoke
# of: sudo bash bench/run.sh --smoke
```

### Volledige meetronde

```bash
# Standaardinstellingen (zie standaarden per workload in run.sh)
just bench-run
# of: sudo bash bench/run.sh

# Met aangepast aantal iteraties
sudo bash bench/run.sh --iter 500 --warmup 50

# Alleen specifieke condities / workloads
sudo bash bench/run.sh --conditions C3,C4,C5 --workloads bulk,ctrl

# Dry-run: print commando's zonder uitvoeren
bash bench/run.sh --dry-run --smoke
```

Resultaten worden weggeschreven naar `results/<ISO-timestamp>/`.

### Handmatig één binary draaien

```bash
# C1 — native libusb
sudo usb-bench-c/build-native/w_ctrl output.csv cafe:4002 100 --condition native-libusb

# C2 — wasi-libusb
sudo usb-wasi-host/target/release/usb-wasi-host \
    -c usb-bench-c/build-wasi/w_ctrl.wasm -- \
    output.csv cafe:4002 100 --condition wasi-libusb

# C3 — native rusb
sudo usb-bench-rs/target/release/w_ctrl output.csv cafe:4002 100 --condition native-rusb

# C4 — wasi-rusb  ← nieuw
sudo usb-wasi-host/target/release/usb-wasi-host \
    -c usb-bench-rs/target-wasi-rusb/wasm32-wasip2/release/w_ctrl.wasm -- \
    output.csv cafe:4002 100 --condition wasi-rusb

# C5 — wasi-raw-wit
sudo usb-wasi-host/target/release/usb-wasi-host \
    -c usb-bench-rs/target/wasm32-wasip2/release/w_ctrl.wasm -- \
    output.csv cafe:4002 100 --condition wasi-raw-wit
```

### CLI-argumenten (alle binaries)

```
<output.csv>  pad naar CSV-uitvoerbestand (wordt aangemaakt of aangevuld)
<VID:PID>     USB Vendor ID en Product ID in hexadecimaal (bv. 054c:0ce6)
<iteraties>   aantal meetiteraties
[--condition <naam>]  overschrijft de standaard conditienaam in de CSV
```

---

## 7. Resultaten analyseren

```bash
# Analyseer de meest recente meetronde
just bench-analyze

# Analyseer een specifieke map
python3 bench/analyze.py results/2026-04-27T20-00-00Z/

# Alleen correctheidscontrole (checksums)
python3 bench/analyze.py results/... --check-only

# Grafieken opslaan naar een map
python3 bench/analyze.py results/... --plots out/figs/
```

Het analyse-script produceert:
1. **Correctheidstabel** — SHA-256 checksums per workload over alle 5 condities
2. **Doorvoer-staafdiagram** — MB/s per conditie (W-bulk, W-iso)
3. **RTT-violinplot** — verdeling per conditie (W-ctrl, W-int)
4. **CPU-gebruik** — user vs sys-tijd per conditie
5. **Memory-gebruik** — RSS-piek + guest-linear-memory (WASM-condities)
6. **Wrapper-overhead** — C4 vs C5 vergelijking (rusb-belasting)
7. **Statistische toetsen** — Mann-Whitney U + Cliff's delta per paar

---

## 8. CSV-schema

Elk meetpunt schrijft één rij:

```
timestamp_iso, condition, workload, iteration,
bytes, duration_ns,
user_cpu_us, sys_cpu_us, rss_peak_kb, guest_mem_bytes,
checksum_hex, notes
```

| Veld | Type | Beschrijving |
|------|------|--------------|
| `timestamp_iso` | string | ISO-8601 tijdstip van de meting |
| `condition` | string | `native-libusb`, `wasi-libusb`, `native-rusb`, `wasi-rusb`, `wasi-raw-wit` |
| `workload` | string | `bulk`, `ctrl`, `int`, `iso` |
| `iteration` | integer | volgnummer (0-gebaseerd) |
| `bytes` | integer | overgedragen bytes in deze iteratie |
| `duration_ns` | integer | RTT in nanoseconden (getimed gedeelte) |
| `user_cpu_us` | integer | user-CPU-tijd delta (µs) via `getrusage` |
| `sys_cpu_us` | integer | sys-CPU-tijd delta (µs) via `getrusage` |
| `rss_peak_kb` | integer | maximale RSS in kB na de iteratie |
| `guest_mem_bytes` | integer | WASM lineair geheugen in bytes (0 voor native) |
| `checksum_hex` | string | SHA-256 van de payload (bulk/iso), leeg voor ctrl/int |
| `notes` | string | vrij veld (leeg tenzij fout) |

---

## 9. Technische diepte: C4-implementatie

### 9.1 Architectuur

```
Rust-broncode (w_bulk.rs, identiek voor C3 en C4)
    │
    ▼
rusb (v0.9)  ──── libusb1-sys (v0.7, via pkg-config)
    │
    ▼
libusb-wasi-rust.a    (libusb API, WIT-backed)
    │
    ├── core.o, io.o, sync.o, ...    (libusb internals)
    ├── wasi_usb.o                   (WASI USB-backend, roept WIT aan)
    └── cguest.o  (gepatcht)         (WIT C-bindingen, import stubs)
          │
          ▼  component:usb/*@0.2.1
    wasmtime host-runtime (usb-wasi-host)
          │
          ▼
    libusb1-sys / OS USB-driver
```

### 9.2 Cargo feature flag

C4 gebruikt de Cargo-feature `c4-rusb-wasi` om `rusb` en `libusb1-sys` ook
beschikbaar te maken als WASM-target (normaal zijn ze native-only):

```toml
# usb-bench-rs/Cargo.toml
[target.'cfg(target_family = "wasm")'.dependencies]
rusb        = { version = "0.9", optional = true }
libusb1-sys = { version = "0.7", optional = true }

[features]
c4-rusb-wasi = ["dep:rusb", "dep:libusb1-sys"]
```

`build-c4.sh` bouwt met `--features c4-rusb-wasi --target wasm32-wasip2`.

### 9.3 pkg-config sysroot

`libusb1-sys`'s `build.rs` gebruikt pkg-config om de libusb-bibliotheek te
vinden. Door `PKG_CONFIG_LIBDIR` naar onze eigen sysroot te wijzen, linkt
Cargo automatisch tegen `libusb-wasi-rust.a` in plaats van de systeem-libusb:

```bash
# bench/build-c4.sh
export PKG_CONFIG_LIBDIR="${SYSROOT}/usr/lib/pkgconfig"
export PKG_CONFIG_SYSROOT_DIR="${SYSROOT}"
export PKG_CONFIG_ALLOW_CROSS=1
```

```ini
# sysroot-wasi/usr/lib/pkgconfig/libusb-1.0.pc
Name: libusb-1.0
Version: 1.0.27
Libs: -L${libusb_root} -lusb-wasi-rust
Cflags: -I${libusb_root}/libusb
```

### 9.4 Waarom `libusb-wasi-rust.a` en niet gewoon `libusb-wasi.a`?

`libusb-wasi.a` werd gebouwd als een **C WASM-component**. Daardoor bevat
`cguest.o` naast de WIT-import stubs ook een run-export handler
(`__wasm_export_exports_wasi_cli_run_run`), die in C de brug vormt tussen
de WASI-component `wasi:cli/run`-export en de C `main()`-functie.

In Rust levert `wasm-component-ld` zelf de `wasi:cli/run`-export via `__main_void`.
Als `cguest.o` ook een run-export probeert aan te bieden, ontstaan er twee problemen:

1. **Onopgelost import**: `cguest.o` importeert `env::exports_wasi_cli_run_run`
   (de C-stijl hook naar `main()`), wat Rust nooit definieert.
2. **Dubbele export**: `wasi:cli/run` wordt door zowel Rust als `cguest.o`
   geëxporteerd, wat de component-encoder afwijst.

**Oplossing**: de run-export handler in `cguest.o` markeren als `VISIBILITY_HIDDEN`
(was `EXPORTED|NO_STRIP`). `--gc-sections` verwijdert dan de handler én zijn
onopgeloste import. De eigenlijke WIT-import stubs (`component_usb_*` functies)
blijven intact, want die worden wél aangeroepen vanuit `wasi_usb.o`.

### 9.5 `guest_component_type.o` als linker-argument

`wasm-component-ld` heeft een *component-type custom section* nodig om te weten
hoe de `env::component_usb_*` interne symbolen uit `cguest.o` gekoppeld worden
aan de WIT-interfacenamen (`component:usb/device@0.2.1` enz.).

Dit object wordt als extra linker-argument meegegeven via `usb-bench-rs/build.rs`:

```rust
// usb-bench-rs/build.rs
if std::env::var("CARGO_CFG_TARGET_OS") == Ok("wasi".into()) {
    let obj = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap()
        .join("usb-bench-c/bindings/guest_component_type.o");
    if obj.exists() {
        println!("cargo:rustc-link-arg={}", obj.display());
    }
}
```

> **Belangrijk**: gebruik `usb-bench-c/bindings/guest_component_type.o`
> en **niet** `rusb-wasi/examples/wasi-workload/wasi-sysroot/usr/lib/cguest_component_type.o`.
> Die laatste vereist `wasi:cli/run@0.2.5`, terwijl Rust 1.93.1 `@0.2.0` levert.

### 9.6 Aparte `--target-dir`

C4 gebruikt `--target-dir target-wasi-rusb` zodat de C4-WASM-binaries
(`w_bulk.wasm`, ...) niet worden overschreven door de C5-WASM-binaries die
op hetzelfde pad terechtkomen als `--target-dir target` gebruikt wordt:

```
usb-bench-rs/
├── target/wasm32-wasip2/release/   ← C5 (wasi-raw-wit)
└── target-wasi-rusb/wasm32-wasip2/release/  ← C4 (wasi-rusb)
```

### 9.7 Transport-selectie in de Rust-broncode

Elke `w_*.rs` binary selecteert de transportlaag via `cfg`-attributen:

```rust
// Geldig voor C3 (native) én C4 (wasm32-wasip2 + feature c4-rusb-wasi)
#[cfg(any(not(target_family = "wasm"), feature = "c4-rusb-wasi"))]
use native::CtrlDevice as ActiveDevice;

// Geldig voor C5 (wasm32-wasip2, zonder c4-rusb-wasi)
#[cfg(all(target_family = "wasm", not(feature = "c4-rusb-wasi")))]
use wasm::CtrlDevice as ActiveDevice;
```

Hierdoor is de **broncode identiek voor C3 en C4** — het enige verschil zit
in de build-configuratie (target + feature + linker), wat de
*source-portability*-claim van de thesis direct bewijst.

---

## 10. Troubleshooting

### `LIBUSB_ERROR_ACCESS` / `Permission denied`

USB-toegang vereist root. Gebruik `sudo` of voeg je gebruiker toe aan de
`plugdev`-groep (Linux):

```bash
sudo usermod -aG plugdev $USER
# Herstart sessie
```

### `ERROR: libusb-wasi-rust.a niet gevonden`

Voer de eenmalige patchstap uit (zie [5.3](#53-c4-specifieke-setup-libusb-wasi-rusta)).

### `ERROR: guest_component_type.o niet gevonden`

Bouw eerst de C2-binaries:
```bash
cmake -B usb-bench-c/build-wasi usb-bench-c \
    -DCMAKE_TOOLCHAIN_FILE=usb-bench-c/toolchain-wasi.cmake
cmake --build usb-bench-c/build-wasi
```

### C4-linker fout: `failed to resolve import env::exports_wasi_cli_run_run`

De `libusb-wasi-rust.a` is niet correct gepatcht of is verouderd.
Herhaal stap [5.3](#53-c4-specifieke-setup-libusb-wasi-rusta).

Verifieer de vlaggen:
```bash
wasm-tools dump libusb/libusb-wasi-rust.a \
    | grep "__wasm_export_exports_wasi_cli_run_run"
# Verwacht: flags: SymbolFlags(VISIBILITY_HIDDEN)
# NIET: EXPORTED of NO_STRIP
```

### C4-linker fout: `failed to find export of wasi:cli/run@0.2.5`

Je gebruikt de verkeerde `cguest_component_type.o`. Controleer `build.rs`:
het moet wijzen naar `usb-bench-c/bindings/guest_component_type.o`, niet naar
`rusb-wasi/examples/wasi-workload/wasi-sysroot/usr/lib/cguest_component_type.o`.

### USB-apparaat niet gevonden tijdens smoke-test

Controleer of het apparaat is aangesloten:
```bash
system_profiler SPUSBDataType | grep -E "Vendor ID|Product ID"  # macOS
lsusb                                                            # Linux
```

Controleer ook of de OS-driver het apparaat niet in bezit heeft:
```bash
# macOS: HID-driver loskoppelen voor PS5 DualSense
# (automatisch bij claim_interface in libusb)

# Linux: USB mass-storage ontkoppelen
sudo umount /dev/sdX
```

### Checksums komen niet overeen tussen condities

Dit duidt op een fout in de data-pad (verkeerd SCSI-commando, verkeerde LBA, ...).
Controleer de `notes`-kolom in de CSV op foutmeldingen.

---

*Gegenereerd als onderdeel van de WASI-USB masterthesis, april 2026.*
