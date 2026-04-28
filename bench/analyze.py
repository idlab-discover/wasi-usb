#!/usr/bin/env python3
"""
bench/analyze.py — WASI-USB thesis data analysis & figures

Usage:
    python3 bench/analyze.py <results-dir> [--plots <out-dir>] [--check-only]

Arguments:
    results-dir    Directory produced by bench/run.sh (contains *.csv files)
    --plots DIR    Write PNG/PDF figures to DIR (default: <results-dir>/plots/)
    --check-only   Only run correctness checks, no plots
    --format FMT   Figure format: png (default) or pdf

Output:
    1. Correctness table — per workload: are W-bulk and W-iso checksums
       consistent across conditions?
    2. Throughput bar chart — MB/s per condition × workload
    3. RTT violin plot — RTT distribution per condition (ctrl, int)
    4. CPU stacked bar — user+sys CPU time per condition
    5. Startup table — first-transfer latency (iteration 0) per condition
    6. Memory bar chart — RSS peak + guest linear memory
    7. Wrapper-overhead figure — C3 vs C5 RTT comparison
    8. Statistical tests — Mann-Whitney U + Cliff's delta for key pairs

Requires: pandas, matplotlib, seaborn, scipy, numpy
Install:  pip install pandas matplotlib seaborn scipy numpy
"""

import argparse
import sys
import os
from pathlib import Path

import numpy as np
import pandas as pd
import matplotlib
matplotlib.use("Agg")   # non-interactive backend
import matplotlib.pyplot as plt
import matplotlib.ticker as ticker
import seaborn as sns
from scipy import stats


# ── Palette & style ──────────────────────────────────────────────────────────

CONDITION_ORDER = ["native-libusb", "wasi-libusb", "native-rusb", "wasi-raw-wit"]
CONDITION_LABELS = {
    "native-libusb": "C1\nnative-libusb",
    "wasi-libusb":   "C2\nwasi-libusb",
    "native-rusb":   "C3\nnative-rusb",
    "wasi-raw-wit":  "C5\nwasi-raw-wit",
}
CONDITION_COLORS = {
    "native-libusb": "#2196F3",   # blue
    "wasi-libusb":   "#64B5F6",   # light blue
    "native-rusb":   "#43A047",   # green
    "wasi-raw-wit":  "#A5D6A7",   # light green
}

WORKLOAD_ORDER = ["bulk", "ctrl", "int", "iso"]
WORKLOAD_LABELS = {
    "bulk": "W-bulk\n(SCSI READ)",
    "ctrl": "W-ctrl\n(Control)",
    "int":  "W-int\n(Interrupt)",
    "iso":  "W-iso\n(Isochronous)",
}

sns.set_theme(style="whitegrid", font_scale=1.1)


# ── CSV loading ───────────────────────────────────────────────────────────────

CSV_DTYPES = {
    "condition":       str,
    "workload":        str,
    "iteration":       "int64",
    "bytes":           "int64",
    "duration_ns":     "int64",
    "user_cpu_us":     "int64",
    "sys_cpu_us":      "int64",
    "rss_peak_kb":     "int64",
    "guest_mem_bytes": "float64",
    "checksum_hex":    str,
    "notes":           str,
}


def load_results(results_dir: Path) -> pd.DataFrame:
    """Load all CSV files in results_dir into a single DataFrame."""
    frames = []
    for csv_path in sorted(results_dir.glob("*.csv")):
        if csv_path.name == "meta.txt":
            continue
        try:
            df = pd.read_csv(csv_path, dtype=str)
            # Coerce numeric columns
            for col, dtype in CSV_DTYPES.items():
                if col in df.columns and dtype != str:
                    df[col] = pd.to_numeric(df[col], errors="coerce")
            frames.append(df)
        except Exception as e:
            print(f"  [warn] Could not load {csv_path.name}: {e}", file=sys.stderr)

    if not frames:
        sys.exit(f"No CSV files found in {results_dir}")

    df = pd.concat(frames, ignore_index=True)
    # Derived columns
    df["rtt_us"]    = df["duration_ns"] / 1_000.0
    df["rtt_ms"]    = df["duration_ns"] / 1_000_000.0
    df["mb_s"]      = (df["bytes"] / 1_048_576.0) / (df["duration_ns"] / 1e9)
    df["total_cpu_us"] = df["user_cpu_us"] + df["sys_cpu_us"]

    # Order categorical columns for consistent plots
    df["condition"] = pd.Categorical(
        df["condition"], categories=CONDITION_ORDER, ordered=True
    )
    df["workload"] = pd.Categorical(
        df["workload"], categories=WORKLOAD_ORDER, ordered=True
    )
    return df


# ── Statistics helpers ────────────────────────────────────────────────────────

def cliffs_delta(a: np.ndarray, b: np.ndarray) -> float:
    """Cliff's delta effect size (−1 … +1)."""
    n1, n2 = len(a), len(b)
    if n1 == 0 or n2 == 0:
        return float("nan")
    dominance = sum(1 if ai > bi else (-1 if ai < bi else 0)
                    for ai in a for bi in b)
    return dominance / (n1 * n2)


def mannwhitney(a: np.ndarray, b: np.ndarray):
    """Mann-Whitney U test; returns (U, p-value, Cliff's delta)."""
    if len(a) < 2 or len(b) < 2:
        return float("nan"), float("nan"), float("nan")
    u, p = stats.mannwhitneyu(a, b, alternative="two-sided")
    d = cliffs_delta(a, b)
    return u, p, d


def effect_label(d: float) -> str:
    ad = abs(d)
    if ad < 0.147:  return "negligible"
    if ad < 0.330:  return "small"
    if ad < 0.474:  return "medium"
    return "large"


def summary_stats(series: pd.Series) -> dict:
    s = series.dropna()
    return {
        "n":      len(s),
        "median": s.median(),
        "mean":   s.mean(),
        "std":    s.std(),
        "p5":     s.quantile(0.05),
        "p95":    s.quantile(0.95),
        "iqr":    s.quantile(0.75) - s.quantile(0.25),
    }


# ── 1. Correctness table ──────────────────────────────────────────────────────

def check_correctness(df: pd.DataFrame) -> bool:
    print("\n━━━ 1. Correctness (checksum consistency) ━━━━━━━━━━━━━━━━━━━━━━━━━━")
    ok = True
    for wl in ["bulk", "iso"]:
        sub = df[(df["workload"] == wl) & df["checksum_hex"].notna() &
                 (df["checksum_hex"] != "")]
        if sub.empty:
            print(f"  {wl:6s}  — no checksum data")
            continue
        unique = sub["checksum_hex"].nunique()
        conds  = sub["condition"].unique().tolist()
        status = "✓ PASS" if unique == 1 else f"✗ FAIL ({unique} distinct checksums)"
        print(f"  {wl:6s}  {status}   conditions: {conds}")
        if unique != 1:
            ok = False
    for wl in ["ctrl", "int"]:
        sub = df[df["workload"] == wl]
        if sub.empty:
            print(f"  {wl:6s}  — no data")
        else:
            print(f"  {wl:6s}  — no checksum (expected)   n={len(sub)}")
    return ok


# ── 2. Throughput bar chart ───────────────────────────────────────────────────

def plot_throughput(df: pd.DataFrame, out_dir: Path, fmt: str):
    tput_wl = ["bulk", "iso"]
    sub = df[df["workload"].isin(tput_wl)].copy()
    if sub.empty:
        print("  [skip] throughput: no bulk/iso data")
        return

    agg = (sub.groupby(["workload", "condition"], observed=True)["mb_s"]
              .agg(["median", "std"])
              .reset_index())
    agg.columns = ["workload", "condition", "median_mb_s", "std_mb_s"]

    fig, axes = plt.subplots(1, len(tput_wl), figsize=(5 * len(tput_wl), 5),
                             sharey=False)
    if len(tput_wl) == 1:
        axes = [axes]

    for ax, wl in zip(axes, tput_wl):
        wl_data = agg[agg["workload"] == wl].sort_values("condition")
        conds   = wl_data["condition"].tolist()
        colors  = [CONDITION_COLORS.get(c, "#888") for c in conds]
        x       = range(len(conds))

        ax.bar(x, wl_data["median_mb_s"], yerr=wl_data["std_mb_s"],
               color=colors, capsize=4, edgecolor="white", linewidth=0.5)
        ax.set_xticks(list(x))
        ax.set_xticklabels([CONDITION_LABELS.get(c, c) for c in conds],
                           fontsize=9)
        ax.set_title(WORKLOAD_LABELS.get(wl, wl), fontsize=11)
        ax.set_ylabel("Throughput (MB/s)")
        ax.yaxis.set_major_formatter(ticker.FormatStrFormatter("%.0f"))

    fig.suptitle("Throughput per condition (median ± std)", fontsize=13, y=1.01)
    fig.tight_layout()
    _save(fig, out_dir / f"throughput.{fmt}")
    print(f"  [plot] throughput → throughput.{fmt}")


# ── 3. RTT violin plot ────────────────────────────────────────────────────────

def plot_rtt(df: pd.DataFrame, out_dir: Path, fmt: str):
    rtt_wl = ["ctrl", "int"]
    sub = df[df["workload"].isin(rtt_wl)].copy()
    if sub.empty:
        print("  [skip] RTT violin: no ctrl/int data")
        return

    fig, axes = plt.subplots(1, len(rtt_wl), figsize=(6 * len(rtt_wl), 5),
                             sharey=False)
    if len(rtt_wl) == 1:
        axes = [axes]

    for ax, wl in zip(axes, rtt_wl):
        wl_data = sub[sub["workload"] == wl]
        conds_present = [c for c in CONDITION_ORDER
                         if c in wl_data["condition"].cat.categories
                         and wl_data[wl_data["condition"] == c].shape[0] > 0]
        if not conds_present:
            ax.set_title(f"{wl} — no data")
            continue

        plot_data = wl_data[wl_data["condition"].isin(conds_present)]
        sns.violinplot(
            data=plot_data,
            x="condition", y="rtt_us",
            order=conds_present,
            palette={c: CONDITION_COLORS.get(c, "#888") for c in conds_present},
            inner="quartile",
            ax=ax,
        )
        ax.set_xticklabels([CONDITION_LABELS.get(c, c) for c in conds_present],
                           fontsize=9)
        ax.set_title(WORKLOAD_LABELS.get(wl, wl), fontsize=11)
        ax.set_xlabel("")
        ax.set_ylabel("RTT (µs)")

    fig.suptitle("RTT distribution per condition", fontsize=13, y=1.01)
    fig.tight_layout()
    _save(fig, out_dir / f"rtt_violin.{fmt}")
    print(f"  [plot] RTT violin → rtt_violin.{fmt}")


# ── 4. CPU stacked bar ────────────────────────────────────────────────────────

def plot_cpu(df: pd.DataFrame, out_dir: Path, fmt: str):
    agg = (df.groupby(["workload", "condition"], observed=True)
             [["user_cpu_us", "sys_cpu_us"]]
             .median()
             .reset_index())

    workloads = [w for w in WORKLOAD_ORDER if w in agg["workload"].values]
    n_wl = len(workloads)
    if n_wl == 0:
        print("  [skip] CPU: no data")
        return

    fig, axes = plt.subplots(1, n_wl, figsize=(4 * n_wl, 5), sharey=False)
    if n_wl == 1:
        axes = [axes]

    for ax, wl in zip(axes, workloads):
        wl_data = agg[agg["workload"] == wl].sort_values("condition")
        conds   = wl_data["condition"].tolist()
        x       = np.arange(len(conds))
        usr     = wl_data["user_cpu_us"].values
        sys_    = wl_data["sys_cpu_us"].values

        ax.bar(x, usr,  label="user",   color="#1976D2", edgecolor="white")
        ax.bar(x, sys_, label="system", color="#F57C00", edgecolor="white",
               bottom=usr)
        ax.set_xticks(x)
        ax.set_xticklabels([CONDITION_LABELS.get(c, c) for c in conds],
                           fontsize=9)
        ax.set_title(WORKLOAD_LABELS.get(wl, wl), fontsize=11)
        ax.set_ylabel("CPU time (µs, median per transfer)")
        if ax == axes[0]:
            ax.legend(fontsize=8)

    fig.suptitle("CPU time per transfer (user + system, median)", fontsize=13,
                 y=1.01)
    fig.tight_layout()
    _save(fig, out_dir / f"cpu_usage.{fmt}")
    print(f"  [plot] CPU usage → cpu_usage.{fmt}")


# ── 5. Startup latency (iteration 0) ─────────────────────────────────────────

def print_startup(df: pd.DataFrame):
    print("\n━━━ 5. Startup latency (iteration 0, per condition × workload) ━━━━━━")
    first = df[df["iteration"] == 0].copy()
    if first.empty:
        print("  No iteration-0 data found")
        return

    tbl = (first.groupby(["workload", "condition"], observed=True)["rtt_us"]
                .first()
                .unstack("condition")
                .reindex(columns=[c for c in CONDITION_ORDER
                                  if c in first["condition"].cat.categories]))
    print(tbl.to_string(float_format=lambda x: f"{x:9.1f} µs"))


# ── 6. Memory bar chart ───────────────────────────────────────────────────────

def plot_memory(df: pd.DataFrame, out_dir: Path, fmt: str):
    # Use last iteration RSS (peak) and guest_mem_bytes
    last = (df.sort_values("iteration")
              .groupby(["workload", "condition"], observed=True)
              .last()
              .reset_index())

    workloads = [w for w in WORKLOAD_ORDER if w in last["workload"].values]
    n_wl = len(workloads)
    if n_wl == 0:
        print("  [skip] memory: no data")
        return

    fig, axes = plt.subplots(1, n_wl, figsize=(4 * n_wl, 5), sharey=False)
    if n_wl == 1:
        axes = [axes]

    for ax, wl in zip(axes, workloads):
        wl_data = last[last["workload"] == wl].sort_values("condition")
        conds   = wl_data["condition"].tolist()
        x       = np.arange(len(conds))
        rss     = wl_data["rss_peak_kb"].values / 1024.0   # → MiB
        # Guest linear memory (only meaningful for WASI conditions)
        guest   = np.where(
            wl_data["guest_mem_bytes"].notna() & (wl_data["guest_mem_bytes"] > 0),
            wl_data["guest_mem_bytes"].fillna(0).values / (1024 * 1024),
            0.0,
        )

        ax.bar(x, rss,   label="RSS peak",    color="#5C6BC0", edgecolor="white")
        ax.bar(x, guest, label="guest linear", color="#EF9A9A", edgecolor="white",
               bottom=rss, alpha=0.8)
        ax.set_xticks(x)
        ax.set_xticklabels([CONDITION_LABELS.get(c, c) for c in conds],
                           fontsize=9)
        ax.set_title(WORKLOAD_LABELS.get(wl, wl), fontsize=11)
        ax.set_ylabel("Memory (MiB)")
        if ax == axes[0]:
            ax.legend(fontsize=8)

    fig.suptitle("Memory usage (RSS peak + guest linear memory)", fontsize=13,
                 y=1.01)
    fig.tight_layout()
    _save(fig, out_dir / f"memory.{fmt}")
    print(f"  [plot] memory → memory.{fmt}")


# ── 7. Wrapper-overhead figure (C3 vs C5) ────────────────────────────────────

def plot_wrapper_overhead(df: pd.DataFrame, out_dir: Path, fmt: str):
    sub = df[df["condition"].isin(["native-rusb", "wasi-raw-wit"])].copy()
    if sub.empty:
        print("  [skip] wrapper overhead: no C3/C5 data")
        return

    workloads = [w for w in WORKLOAD_ORDER if w in sub["workload"].values]
    n_wl = len(workloads)
    fig, axes = plt.subplots(1, n_wl, figsize=(4 * n_wl, 5), sharey=False)
    if n_wl == 1:
        axes = [axes]

    for ax, wl in zip(axes, workloads):
        wl_data = sub[sub["workload"] == wl]
        for cond in ["native-rusb", "wasi-raw-wit"]:
            s = wl_data[wl_data["condition"] == cond]["rtt_us"].dropna().values
            if len(s) == 0:
                continue
            xs = np.sort(s)
            ys = np.arange(1, len(xs) + 1) / len(xs)
            ax.plot(xs, ys, label=CONDITION_LABELS.get(cond, cond),
                    color=CONDITION_COLORS.get(cond, "#888"), linewidth=1.5)
        ax.set_title(WORKLOAD_LABELS.get(wl, wl), fontsize=11)
        ax.set_xlabel("RTT (µs)")
        ax.set_ylabel("CDF")
        ax.legend(fontsize=8)

    fig.suptitle("Wrapper overhead: C3 (native-rusb) vs C5 (wasi-raw-wit)\n"
                 "CDF of per-transfer RTT", fontsize=12, y=1.02)
    fig.tight_layout()
    _save(fig, out_dir / f"wrapper_overhead.{fmt}")
    print(f"  [plot] wrapper overhead → wrapper_overhead.{fmt}")


# ── 8. Statistical tests ──────────────────────────────────────────────────────

PAIRS = [
    ("native-libusb", "wasi-libusb",  "C1↔C2  WASI cost (C)"),
    ("native-rusb",   "wasi-raw-wit", "C3↔C5  WASI cost (Rust)"),
    ("native-libusb", "native-rusb",  "C1↔C3  Language effect (native)"),
    ("wasi-libusb",   "wasi-raw-wit", "C2↔C5  Language effect (WASI)"),
]


def print_stats(df: pd.DataFrame):
    print("\n━━━ 8. Statistical tests (Mann-Whitney U + Cliff's delta) ━━━━━━━━━━")
    for wl in WORKLOAD_ORDER:
        sub = df[df["workload"] == wl]
        if sub.empty:
            continue
        print(f"\n  Workload: {wl}")
        print(f"  {'Comparison':<38} {'n_a':>6} {'n_b':>6} "
              f"{'U':>10} {'p':>10} {'delta':>8}  effect")
        print("  " + "─" * 90)
        for cond_a, cond_b, label in PAIRS:
            a = sub[sub["condition"] == cond_a]["rtt_us"].dropna().values
            b = sub[sub["condition"] == cond_b]["rtt_us"].dropna().values
            if len(a) < 2 or len(b) < 2:
                print(f"  {label:<38}  — insufficient data")
                continue
            u, p, d = mannwhitney(a, b)
            eff = effect_label(d)
            p_str = f"{p:.2e}" if not np.isnan(p) else "n/a"
            print(f"  {label:<38} {len(a):>6} {len(b):>6} "
                  f"{u:>10.0f} {p_str:>10} {d:>+8.3f}  {eff}")


# ── Summary table ─────────────────────────────────────────────────────────────

def print_summary(df: pd.DataFrame):
    print("\n━━━ Descriptive statistics (RTT µs, per workload × condition) ━━━━━━")
    for wl in WORKLOAD_ORDER:
        sub = df[df["workload"] == wl]
        if sub.empty:
            continue
        print(f"\n  {wl}:")
        print(f"  {'condition':<18} {'n':>6} {'median':>9} {'mean':>9} "
              f"{'std':>9} {'p5':>9} {'p95':>9}")
        print("  " + "─" * 70)
        for cond in CONDITION_ORDER:
            s = sub[sub["condition"] == cond]["rtt_us"].dropna()
            if s.empty:
                continue
            st = summary_stats(s)
            print(f"  {cond:<18} {st['n']:>6} {st['median']:>9.1f} "
                  f"{st['mean']:>9.1f} {st['std']:>9.1f} "
                  f"{st['p5']:>9.1f} {st['p95']:>9.1f}")


# ── Save helper ───────────────────────────────────────────────────────────────

def _save(fig, path: Path):
    path.parent.mkdir(parents=True, exist_ok=True)
    fig.savefig(path, bbox_inches="tight", dpi=150)
    plt.close(fig)


# ── CLI ───────────────────────────────────────────────────────────────────────

def parse_args():
    p = argparse.ArgumentParser(
        description="WASI-USB benchmark analysis",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    p.add_argument("results_dir", help="Directory with CSV files from bench/run.sh")
    p.add_argument("--plots", metavar="DIR",
                   help="Output directory for figures (default: <results_dir>/plots)")
    p.add_argument("--check-only", action="store_true",
                   help="Only run correctness checks, skip plots")
    p.add_argument("--format", default="png", choices=["png", "pdf"],
                   metavar="FMT", dest="fmt",
                   help="Figure file format (default: png)")
    return p.parse_args()


def main():
    args = parse_args()
    results_dir = Path(args.results_dir).resolve()
    if not results_dir.is_dir():
        sys.exit(f"results_dir not found: {results_dir}")

    plots_dir = Path(args.plots).resolve() if args.plots \
                else results_dir / "plots"

    print(f"Loading results from: {results_dir}")
    df = load_results(results_dir)
    print(f"  Loaded {len(df)} rows  "
          f"({df['condition'].nunique()} conditions × "
          f"{df['workload'].nunique()} workloads)")

    # 1. Correctness
    check_correctness(df)

    # 5. Startup table (text only)
    print_startup(df)

    # 8. Statistical tests (text only)
    print_summary(df)
    print_stats(df)

    if args.check_only:
        print("\n[--check-only] Skipping plots.")
        return

    print(f"\nWriting figures to: {plots_dir}")
    plots_dir.mkdir(parents=True, exist_ok=True)

    # 2. Throughput
    plot_throughput(df, plots_dir, args.fmt)

    # 3. RTT violins
    plot_rtt(df, plots_dir, args.fmt)

    # 4. CPU usage
    plot_cpu(df, plots_dir, args.fmt)

    # 6. Memory
    plot_memory(df, plots_dir, args.fmt)

    # 7. Wrapper overhead
    plot_wrapper_overhead(df, plots_dir, args.fmt)

    print(f"\n✓ Done — figures in {plots_dir}")


if __name__ == "__main__":
    main()
