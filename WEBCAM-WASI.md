# USB-camera via WASI-USB: architectuur en beperkingen

> **Doel van dit document:** vereiste documentatie voor het werkplan —
> "documenteren van de interactie tussen webcam-module, host-runtime en
> USB-backend, inclusief beperkingen (bandbreedte, frame rate, bufferformaten)".

---

## 1. Architectuuroverzicht

```
┌─────────────────────────────────────────────────────────────────────┐
│  Guest (WASM component)                                             │
│                                                                     │
│  webcam.rs                                                          │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │  1. list_devices()  → zoek Brio 100 (046d:094c)              │  │
│  │  2. open() → device handle                                    │  │
│  │  3. get_active_configuration_descriptor()                     │  │
│  │  4. claim_interface(iface=1)  [VideoStreaming]                │  │
│  │  5. Control: UVC Probe (VS_PROBE_CONTROL GET_CUR / SET_CUR)   │  │
│  │     → onderhandel framerate, formaat (MJPEG 640×360 @30fps)   │  │
│  │  6. Control: UVC Commit (VS_COMMIT_CONTROL SET_CUR)           │  │
│  │     → activeer streaming in de camera                         │  │
│  │  7. set_interface_altsetting(iface=1, alt=1)                  │  │
│  │     → schakel over naar alt-setting met iso endpoints         │  │
│  │  8. Loop:                                                      │  │
│  │     a. new_transfer(Isochronous, ep=0x81, buf=32 KiB, pkts=32)│  │
│  │     b. submit_transfer()                                       │  │
│  │     c. await_transfer() → TransferResult{data, packets[]}     │  │
│  │     d. UVC payload header stripping (FID-bit-gebaseerd)       │  │
│  │     e. Frame reassembly (append tot frame buffer tot FID flip) │  │
│  │     f. is_complete_jpeg() → emit RawFrame{data, 640, 360}     │  │
│  └──────────────────────────────────────────────────────────────┘  │
│                                                                     │
│  WIT-interface: component:usb/device@0.2.1                         │
│               + component:usb/transfers@0.2.1                      │
└───────────────────────────┬─────────────────────────────────────────┘
                            │ WIT host calls (via Wasmtime component model)
┌───────────────────────────▼─────────────────────────────────────────┐
│  Host: usb-wasi-host (Rust + Wasmtime)                              │
│                                                                     │
│  main.rs / usb_backend.rs                                           │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │  claim_interface → libusb_claim_interface(iface=1)           │  │
│  │  control OUT     → libusb_control_transfer (UVC probe/commit)│  │
│  │  set_alt_setting → libusb_set_interface_alt_setting          │  │
│  │  new_transfer    → libusb_alloc_transfer(iso_packets=32)     │  │
│  │  submit_transfer → libusb_submit_transfer                    │  │
│  │  await_transfer  → tokio oneshot::channel + callback         │  │
│  └──────────────────────────────────────────────────────────────┘  │
│                                                                     │
│  OS USB-stack (libusb1-sys / libusb 1.0.x)                         │
└───────────────────────────┬─────────────────────────────────────────┘
                            │ USB 2.0 HS isochronous endpoint
┌───────────────────────────▼─────────────────────────────────────────┐
│  Logitech Brio 100 webcam (046d:094c)                               │
│  MJPEG output @ 640×360, 30 fps                                     │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 2. UVC-protocol in het guest

De guest implementeert de volledige UVC Probe/Commit-handshake in WASM zonder
enige UVC-kennis in de host. Dit illustreert het "dumb host, smart guest"-principe:

### 2.1 Probe/Commit-volgorde

```
Guest → Host (control OUT):   VS_PROBE_CONTROL SET_CUR
  bmHint = 0x01 (dwFrameInterval is vast)
  bFormatIndex = 1  (MJPEG)
  bFrameIndex  = 1  (640×360)
  dwFrameInterval = 333333  (= 1/30 s × 10^7, in 100 ns eenheden)

Host → Guest (control IN):    VS_PROBE_CONTROL GET_CUR
  → teruggestuurde parameters (wat camera daadwerkelijk ondersteunt)

Guest → Host (control OUT):   VS_COMMIT_CONTROL SET_CUR
  → zet de onderhandelde parameters vast
  → camera begint intern te streamen
```

### 2.2 Frame reassembly

UVC stuurt per isochronous transfer 0–N UVC-payloads. Elke payload heeft een
2-byte header:

```
byte 0: Header Length (HLE, typisch 2 of 12)
byte 1: bmHeaderInfo
  bit 0: FID  (Frame IDentifier — togglet bij elke nieuwe frame)
  bit 1: EOF  (end of frame)
  bit 2: PTS  (Presentation Time Stamp aanwezig)
  bit 3: SCR  (Source Clock Reference aanwezig)
  bit 6: ERR  (payload error)
  bit 7: EOH  (end of header)
```

De guest detecteert een nieuwe frame wanneer het FID-bit van 0→1 of 1→0
schakelt. Data na het header-veld wordt toegevoegd aan de frame-buffer totdat
de frame compleet is (FID flip of JPEG EOI marker `0xFF 0xD9`).

---

## 3. Bufferformaten

| Parameter | Waarde | Toelichting |
|-----------|--------|-------------|
| Transfer type | Isochronous | USB spec: vaste bandbreedte, geen retransmit |
| Endpoint | `0x81` (IN) | iso-IN endpoint van de Brio 100 |
| Packets per transfer | 32 | Elke `libusb_alloc_transfer(32)` aanroep |
| Packet size | 1024 B | `wMaxPacketSize` voor HS (USB 2.0 High-Speed) |
| Buffer per transfer | 32 × 1024 = **32 KiB** | Totale allocatie per submit |
| Frame size (MJPEG 640×360) | ≈ 15–40 KiB | Afhankelijk van scène-complexiteit |
| Frames per second | 30 | Onderhandeld via UVC probe/commit |
| Theoretische bandbreedte | 1024 B × 32 pkts × (1000/8 ms) ≈ **4 MB/s** | Op USB 2.0 HS micro-frame basis |
| Gemeten (Linux, verwacht) | 3–6 MB/s sustained | MJPEG compressie varieert per frame |

**Noot over packet-size:** `wMaxPacketSize` voor HS isochronous kan tot
1024 bytes zijn bij alt-setting 1. Op macOS meldt de driver soms een kleinere
waarde door de sandboxing van de IOUSBDeviceFamily driver.

---

## 4. Bandbreedte-berekening

USB 2.0 High-Speed isochronous:
- 1 micro-frame = 125 µs = 1/8000 s
- Maximum bytes per micro-frame: 1024 B (voor 1 transactie per micro-frame)
- 32 packets à 1024 B per transfer = 32768 B per submit
- Theoretisch maximum voor HS iso-IN: 1024 × 8000 = **8.19 MB/s**

In de praktijk (MJPEG 640×360 @30fps):
- 1 frame ≈ 25 KiB MJPEG (gemiddeld voor een matig complexe scène)
- 30 frames/s × 25 KiB = 750 KiB/s effectieve payload
- Maar de bus wordt gereserveerd voor de maximale packet-size ongeacht of de
  camera data heeft → overhead factor ~10×
- Werkelijke bus-utilization: ≈ 10–15%

---

## 5. Latency-model

```
Guest submit_transfer()
  │
  ├─ WIT host call (Wasmtime boundary crossing)         ~2–10 µs
  │
  ├─ libusb_alloc_transfer + libusb_submit_transfer     ~1–5 µs
  │
  ├─ OS USB-stack queues transfer in hardware            ~0 µs (async)
  │
  ├─ USB micro-frame timing (125 µs intervals)          0–125 µs wachttijd
  │
  ├─ Camera levert data in 1–4 micro-frames             125–500 µs
  │
  ├─ transfer_callback fired                             ~1 µs
  │
  └─ await_transfer() returns                            ~2–10 µs (WIT + tokio)

Totale RTT per transfer (verwacht op Linux): 300–700 µs
```

---

## 6. Beperkingen

### 6.1 macOS (huidige ontwikkelmachine)

| Beperking | Oorzaak | Gevolg |
|-----------|---------|--------|
| 0 bytes per transfer | `IOUSBDeviceFamily` houdt UVC interface exclusief | Alle iso-data is leeg |
| Transfers time-out na 5 s | macOS laat `libusb_detach_kernel_driver` niet toe | Meting onmogelijk |
| `libusb_claim_interface` faalt | Apple's UVC driver bezet de interface | `LIBUSB_ERROR_ACCESS` |

**Oplossing:** meetdata vereist **Linux** waar `libusb_detach_kernel_driver("uvcvideo")` werkt.

### 6.2 WASI-specifieke beperkingen

| Beperking | Oorzaak | Impact |
|-----------|---------|--------|
| Buffers zijn kopieën | WIT marshalling: guest heap → host heap → camera → host heap → guest heap | ~2× geheugengebruik t.o.v. native zero-copy |
| Geen DMA zero-copy | WASM linear memory is niet pinnable voor DMA | Minimale extra latency per transfer (~2 µs) |
| Synchrone await | `await_transfer` blokkeert de WASM-thread | Geen parallelle submits vanuit C-guest (Rust: tokio-async mogelijk) |

### 6.3 UVC protocol-beperkingen

| Beperking | Gevolg |
|-----------|--------|
| Geen error recovery in iso | Pakketverlies → frame corrupt → JPEG-parser verwerpt frame |
| FID-gebaseerde assembly | Frame boundary detectie werkt alleen als FID correct togglet |
| MJPEG-variabele grootte | Frame-grootte afhankelijk van scène → throughput varieert ±3× |

---

## 7. Verwachte meetresultaten (Linux, W-iso benchmark)

Op basis van de C2/C4/C5-implementatie + UVC-spec verwachten we:

| Conditie | Sustained throughput | RTT per transfer | Frames/s |
|----------|---------------------|------------------|---------|
| C1 native-libusb | 4–6 MB/s | 300–600 µs | ~30 fps |
| C2 wasi-libusb | 3–5 MB/s | 400–800 µs | ~25–30 fps |
| C3 native-rusb | 4–6 MB/s | 300–600 µs | ~30 fps |
| C4 wasi-rusb | 3–5 MB/s | 400–800 µs | ~25–30 fps |
| C5 wasi-raw-WIT | 3–5 MB/s | 350–700 µs | ~25–30 fps |

**WASI-overhead verwachting:** +100–200 µs per transfer t.o.v. native, voornamelijk:
- WIT-grensovergang (Wasmtime component model): ~50–100 µs
- Buffer-kopiëring (guest→host, host→guest): ~50–100 µs
- Geen significant verschil C2/C4/C5 onderling voor iso (bottleneck = USB-bus, niet WIT)

---

## 8. Relevante broncode

| Bestand | Wat het doet |
|---------|-------------|
| `usb-wasi-guest/examples/webcam/src/webcam.rs` | UVC probe/commit, iso-submit-loop, frame-assembly |
| `usb-wasi-guest/examples/webcam/src/main.rs` | Entrypoint, output naar `out/frame_*.jpg` |
| `usb-bench-c/src/w_iso.c` | ISO benchmark (geen UVC-parsing, meet rauwe bytes) |
| `usb-bench-rs/src/bin/w_iso.rs` | Rust-equivalent van `w_iso.c` voor C3/C4/C5 |
| `usb-wasi-host/src/main.rs` | ISO transfer callback, per-packet status gathering |
| `libusb/libusb/os/wasi_usb.c` | WASI backend: submit/await isochrone transfers |
