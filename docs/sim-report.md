# Simulator report

Findings first, then the generated tables. Regenerate the tables with `cargo run --release -p globalobject-sim`.
Seeds are fixed, so runs repeat exactly.

## What this changes

| Question from the plan | Answer | Confidence |
|---|---|---|
| Does the pool stall without `headroom`? | **Yes.** It stops at 3.2% full. It needs `headroom × (1 − reserve) > 1` (section 7) | High: matches the algebra |
| Is EC more available than 3 copies? | **Not in realistic conditions.** See below | Medium: depends on the churn model |
| Is a 40-host switch to EC right? | Reasonable, but **it must also depend on uptime** | Medium |
| Grace period? | **Keep 72 h** (section 4) | Medium |
| Placement group size? | **Keep 16 or more** (section 5) | Medium |
| Is repair bandwidth a problem? | **No**, except during backlogs (below) | Medium |
| Reserve size? | **Keep 20%.** A full pool survives losing a share of hosts up to its reserve (section 8) | High: `r ≥ l` algebraically |
| Can anyone restore in reasonable time? | **Slowly.** 4.6 days per TB on 20 Mbps up without banked credit (section 11) | High: arithmetic |

### EC's advantage isn't availability

The plan's table, and the question I asked when choosing redundancy, compared 1-of-3 with 10-of-20 at
77% against 99.98%. That table assumed independent failures and independently placed chunks. With
correlated churn, placement groups and repair, the home mix gives:

| Policy | 1 GiB file readable | Usable per TB (20% reserve) | Lost per year | Repair Mbps/host |
|---|---|---|---|---|
| 1-of-3 | 98.6% | 267 GB | 0.0003% (2 of 8 runs) | 0.050 |
| 1-of-4 | 99.4% | 200 GB | none seen | 0.033 |
| 1-of-5 | 99.9% | 160 GB | none seen | 0.036 |
| 10-of-20 | 98.1% | 400 GB | none seen | 0.081 |
| 10-of-16 | 85.0% | 500 GB | 0.0047% (2 of 8 runs) | 0.114 |

Two things cause the gap. Placement groups largely remove the "many chunks, many chances to fail" penalty
that hurt 1-of-3 in the analytic table. And 10-of-20 repairs lazily, so it often runs with several pieces
already missing when an outage hits.

EC still earns its place: **50% more usable space** than 1-of-3, no loss seen in these runs, and pieces that
can't be deduped. Availability just isn't the reason, and 10-of-16 should not be offered.

### EC fails badly on low-uptime hosts

With mostly laptops (52% mean uptime), 10-of-20 falls to 24% of files readable and 10-of-16 to 4%, while 1-of-3
holds 89% and 1-of-5 96%. A code can't work when uptime is near or below `k/n`.

**Rule to adopt:** switch a pool to EC only with at least 40 eligible owners **and** mean measured uptime of
at least 75%. Otherwise stay on replication. The uptime threshold is a judgement from the 52% and 81% results. There
is no run between them, so it needs refining.

### Durability is not yet good enough to promise

1-of-3 at 0.0003% of segments per year is about 0.045 lost segments per year per TB stored (15,000 segments of
64 MiB), though loss arrives in groups, not evenly. A lost segment damages every file with a chunk in it. That
rests on just 2 runs out of 8 showing any loss, so the figure itself is rough.

"None seen" for other policies means none in the baseline's 8 runs of 3 years, not that the risk is zero: 10-of-20
did lose data in a few other configurations, such as 100%/yr host loss and small placement groups. These runs can't
measure very rare events, and rare events are what durability is about. **Don't advertise a durability figure from this.
Tell users to keep an independent copy of anything irreplaceable.**

Repair is quick enough that the 2 Mbps/host budget is never the limit in steady state. Sustained traffic stays
under 0.5 Mbps per host even at 100%/yr host loss.

### Recommended defaults

| Setting | Value | Basis |
|---|---|---|
| Below 40 owners, or uptime under 75% | `1-of-3`, repair below 3 | Your floor of three disks |
| 40+ owners and uptime 75%+ | `10-of-20`, repair below 15 | 400 GB per TB, no loss seen |
| Offer for important data | `1-of-4` or `1-of-5` | Best availability and no loss seen, at a cost of space |
| Never offer | `10-of-16` | Poor availability and real loss |
| Grace | 72 h | Under 72 h, repair traffic and peak backlog grow quickly. Past 168 h, 1-of-3 loses more |
| Placement group | 16 segments | 96.9% at 1, 98.6% at 16, little more above that |
| Reserve | 20% | Recovers from losing 20% of hosts at once |
| Headroom | 1.5 | 1.3 is the lowest that fills at 20% reserve. Lower is safer against false pledges (51% overdraft at 1.5, 30% at 1.3), but this model doesn't measure how quickly a real pool grows, so 1.5 stays for now |

## Limits of these results

- **The churn model is invented, not measured.** Class mixes, session lengths, 6 regions and one regional outage a
  year are plausible guesses. Real measurements from a beta should replace them, and they could change the
  conclusions above.
- **Host loss is independent.** No correlated permanent loss, such as a bad firmware or a mass exit.
- **No malicious behaviour** in the churn runs. Section 10 treats attacks separately, mostly analytically.
- **Reads** are modelled as instantaneous, and bandwidth contention isn't simulated. Repair budget is the only
  bandwidth limit in the model.
- **Pool-full recovery** in section 8 is arithmetic, not simulated.
- **Scaling** treats each host as holding 800 GB by scaling traffic. The real number of segments tracked is far lower.
- **Runs at 10,000+ hosts** used 2 seeds and 1 year, so treat them as a sanity check.

## How the simulator was checked

- Section 1 reproduces the analytic table to within 0.52 percentage points.
- `repair_keeps_up_with_host_loss` compares repair traffic against the theoretical `loss rate × data held`.
- `nightly_shutdowns_alone_trigger_no_repair` checks grace behaviour and uptime.

Those checks found two real simulator bugs during development: a dead host's pending shutdown cancelled its
repair timer, and an event-time macro drew randomness twice, processing events past the end of the run. Before the
fix, loss looked around 1,000 times worse than it is. Earlier numbers from the run should be disregarded.

---

## 1. Validation against the analytic table

Independent failures, 256 independently placed chunks per file, 200-host pool.

| Policy | Uptime | Analytic | Simulated | Diff |
|---|---|---|---|---|
| 1-of-3 | 80% | 12.79% | 12.87% | +0.07 pt |
| 1-of-3 | 90% | 77.40% | 77.57% | +0.17 pt |
| 1-of-3 | 95% | 96.85% | 96.78% | -0.07 pt |
| 1-of-5 | 80% | 92.13% | 91.90% | -0.23 pt |
| 1-of-5 | 90% | 99.74% | 99.78% | +0.04 pt |
| 1-of-5 | 95% | 99.99% | 100.00% | +0.01 pt |
| 6-of-12 | 80% | 36.75% | 37.26% | +0.52 pt |
| 6-of-12 | 90% | 98.72% | 98.60% | -0.12 pt |
| 6-of-12 | 95% | 99.99% | 99.98% | -0.01 pt |
| 10-of-16 | 80% | 0.10% | 0.10% | -0.00 pt |
| 10-of-16 | 90% | 87.88% | 87.74% | -0.14 pt |
| 10-of-16 | 95% | 99.85% | 99.86% | +0.01 pt |
| 10-of-20 | 80% | 86.57% | 86.07% | -0.50 pt |
| 10-of-20 | 90% | 99.98% | 99.98% | +0.00 pt |
| 10-of-20 | 95% | 100.00% | 100.00% | +0.00 pt |

Largest difference: 0.52 percentage points. This validates the sampling, and independent failures only.

## 2. Policy against churn (200 hosts, 3 years, 72 h grace, 20%/yr host loss)

Regions are time zones. Nightly hosts in one region shut down together, and each region has one 12-48 hour outage a year in the non-always-on mixes.

**always-on**

| Case | Segments lost per year | Runs with loss | Segment unavailable | 1 GiB file readable | Mean uptime | Repair Mbps/host (800 GB held) | Peak backlog |
|---|---|---|---|---|---|---|---|
| 1-of-3 | 0.0000% | 0/8 | 0.000% | 100.000% | 99% | 0.041 | 954 |
| 1-of-4 | 0.0000% | 0/8 | 0.000% | 100.000% | 99% | 0.027 | 487 |
| 1-of-5 | 0.0000% | 0/8 | 0.000% | 100.000% | 99% | 0.030 | 587 |
| 6-of-12 | 0.0000% | 0/8 | 0.000% | 100.000% | 99% | 0.061 | 909 |
| 10-of-16 | 0.0000% | 0/8 | 0.000% | 100.000% | 99% | 0.096 | 1673 |
| 10-of-20 | 0.0000% | 0/8 | 0.000% | 100.000% | 99% | 0.069 | 1156 |

**home mix (60/30/10)**

| Case | Segments lost per year | Runs with loss | Segment unavailable | 1 GiB file readable | Mean uptime | Repair Mbps/host (800 GB held) | Peak backlog |
|---|---|---|---|---|---|---|---|
| 1-of-3 | 0.0003% | 2/8 | 0.212% | 98.581% | 81% | 0.050 | 1454 |
| 1-of-4 | 0.0000% | 0/8 | 0.132% | 99.387% | 81% | 0.033 | 509 |
| 1-of-5 | 0.0000% | 0/8 | 0.019% | 99.898% | 81% | 0.036 | 585 |
| 6-of-12 | 0.0000% | 0/8 | 1.065% | 96.062% | 81% | 0.073 | 706 |
| 10-of-16 | 0.0047% | 2/8 | 4.024% | 84.997% | 81% | 0.114 | 1600 |
| 10-of-20 | 0.0000% | 0/8 | 0.532% | 98.138% | 81% | 0.081 | 1140 |

**mostly laptops (20/30/50)**

| Case | Segments lost per year | Runs with loss | Segment unavailable | 1 GiB file readable | Mean uptime | Repair Mbps/host (800 GB held) | Peak backlog |
|---|---|---|---|---|---|---|---|
| 1-of-3 | 0.0003% | 2/8 | 1.482% | 88.947% | 52% | 0.098 | 1821 |
| 1-of-4 | 0.0000% | 0/8 | 2.294% | 88.764% | 52% | 0.061 | 475 |
| 1-of-5 | 0.0000% | 0/8 | 0.786% | 95.841% | 52% | 0.065 | 607 |
| 6-of-12 | 0.0028% | 1/8 | 31.665% | 28.347% | 52% | 0.104 | 625 |
| 10-of-16 | 0.1561% | 8/8 | 64.713% | 4.293% | 52% | 0.160 | 1833 |
| 10-of-20 | 0.0028% | 1/8 | 37.222% | 24.041% | 52% | 0.100 | 1417 |

## 3. Pool size (home mix, 3 years)

| Case | Segments lost per year | Runs with loss | Segment unavailable | 1 GiB file readable | Mean uptime | Repair Mbps/host (800 GB held) | Peak backlog |
|---|---|---|---|---|---|---|---|
| 5 hosts, 1-of-3 | 0.0000% | 0/8 | 0.035% | 99.871% | 84% | 0.053 | 539 |
| 5 hosts, 1-of-5 | 0.0000% | 0/8 | 0.000% | 100.000% | 84% | 0.025 | 600 |
| 5 hosts, 10-of-20 | not enough hosts | | | | | | |
| 10 hosts, 1-of-3 | 0.0000% | 0/8 | 0.075% | 99.686% | 83% | 0.051 | 721 |
| 10 hosts, 1-of-5 | 0.0000% | 0/8 | 0.003% | 99.989% | 83% | 0.034 | 564 |
| 10 hosts, 10-of-20 | not enough hosts | | | | | | |
| 20 hosts, 1-of-3 | 0.0000% | 0/8 | 0.122% | 99.253% | 83% | 0.059 | 870 |
| 20 hosts, 1-of-5 | 0.0000% | 0/8 | 0.014% | 99.942% | 83% | 0.034 | 778 |
| 20 hosts, 10-of-20 | 0.0000% | 0/8 | 1.486% | 98.506% | 83% | 0.069 | 2400 |
| 30 hosts, 1-of-3 | 0.0000% | 0/8 | 0.163% | 98.893% | 82% | 0.047 | 1082 |
| 30 hosts, 1-of-5 | 0.0000% | 0/8 | 0.016% | 99.928% | 82% | 0.033 | 515 |
| 30 hosts, 10-of-20 | 0.0000% | 0/8 | 0.924% | 96.526% | 82% | 0.092 | 1176 |
| 40 hosts, 1-of-3 | 0.0000% | 0/8 | 0.150% | 99.149% | 81% | 0.050 | 876 |
| 40 hosts, 1-of-5 | 0.0000% | 0/8 | 0.024% | 99.871% | 81% | 0.041 | 601 |
| 40 hosts, 10-of-20 | 0.0000% | 0/8 | 0.401% | 98.751% | 81% | 0.063 | 826 |
| 60 hosts, 1-of-3 | 0.0012% | 1/8 | 0.143% | 98.982% | 81% | 0.053 | 967 |
| 60 hosts, 1-of-5 | 0.0000% | 0/8 | 0.016% | 99.915% | 81% | 0.037 | 685 |
| 60 hosts, 10-of-20 | 0.0000% | 0/8 | 0.558% | 98.132% | 81% | 0.074 | 1189 |
| 100 hosts, 1-of-3 | 0.0000% | 0/8 | 0.221% | 98.483% | 80% | 0.054 | 1003 |
| 100 hosts, 1-of-5 | 0.0000% | 0/8 | 0.022% | 99.864% | 81% | 0.035 | 664 |
| 100 hosts, 10-of-20 | 0.0000% | 0/8 | 0.725% | 97.602% | 81% | 0.078 | 710 |
| 1000 hosts, 1-of-3 | 0.0003% | 3/8 | 0.171% | 98.767% | 82% | 0.049 | 347 |
| 1000 hosts, 1-of-5 | 0.0000% | 0/8 | 0.022% | 99.896% | 82% | 0.036 | 278 |
| 1000 hosts, 10-of-20 | 0.0000% | 0/8 | 0.457% | 98.383% | 82% | 0.077 | 202 |
| 10000 hosts, 1-of-3 | 0.0000% | 0/2 | 0.255% | 98.771% | 82% | 0.060 | 34 |
| 10000 hosts, 1-of-5 | 0.0000% | 0/2 | 0.021% | 99.929% | 82% | 0.031 | 16 |
| 10000 hosts, 10-of-20 | 0.0000% | 0/2 | 0.468% | 99.091% | 82% | 0.035 | 32 |
| 100000 hosts, 1-of-3 | 0.0000% | 0/2 | 0.261% | 98.866% | 82% | 0.063 | 57 |
| 100000 hosts, 1-of-5 | 0.0000% | 0/2 | 0.024% | 99.926% | 82% | 0.032 | 16 |
| 100000 hosts, 10-of-20 | 0.0000% | 0/2 | 0.447% | 99.138% | 82% | 0.035 | 48 |

## 4. Grace period (1-of-3 and 10-of-20, home mix)

| Case | Segments lost per year | Runs with loss | Segment unavailable | 1 GiB file readable | Mean uptime | Repair Mbps/host (800 GB held) | Peak backlog |
|---|---|---|---|---|---|---|---|
| 12 h, 1-of-3 | 0.0000% | 0/8 | 0.268% | 97.378% | 81% | 0.105 | 17739 |
| 12 h, 10-of-20 | 0.0000% | 0/8 | 0.077% | 99.542% | 81% | 0.188 | 20313 |
| 24 h, 1-of-3 | 0.0000% | 0/8 | 0.191% | 98.502% | 81% | 0.064 | 11555 |
| 24 h, 10-of-20 | 0.0000% | 0/8 | 0.118% | 99.388% | 81% | 0.150 | 13126 |
| 72 h, 1-of-3 | 0.0003% | 2/8 | 0.212% | 98.581% | 81% | 0.050 | 1454 |
| 72 h, 10-of-20 | 0.0000% | 0/8 | 0.532% | 98.138% | 81% | 0.081 | 1140 |
| 168 h, 1-of-3 | 0.0007% | 2/8 | 0.565% | 97.145% | 81% | 0.041 | 957 |
| 168 h, 10-of-20 | 0.0000% | 0/8 | 1.085% | 96.461% | 81% | 0.069 | 1222 |
| 336 h, 1-of-3 | 0.0024% | 7/8 | 0.622% | 96.899% | 81% | 0.040 | 940 |
| 336 h, 10-of-20 | 0.0000% | 0/8 | 1.132% | 96.365% | 81% | 0.068 | 1299 |

## 5. Placement group size (home mix)

| Case | Segments lost per year | Runs with loss | Segment unavailable | 1 GiB file readable | Mean uptime | Repair Mbps/host (800 GB held) | Peak backlog |
|---|---|---|---|---|---|---|---|
| 1 segs/group, 1-of-3 | 0.0000% | 0/8 | 0.200% | 96.924% | 81% | 0.050 | 1163 |
| 1 segs/group, 10-of-20 | 0.0000% | 0/8 | 0.589% | 92.581% | 81% | 0.078 | 1267 |
| 4 segs/group, 1-of-3 | 0.0002% | 1/8 | 0.196% | 98.117% | 81% | 0.050 | 1044 |
| 4 segs/group, 10-of-20 | 0.0007% | 1/8 | 0.597% | 96.642% | 81% | 0.073 | 740 |
| 16 segs/group, 1-of-3 | 0.0003% | 2/8 | 0.212% | 98.581% | 81% | 0.050 | 1454 |
| 16 segs/group, 10-of-20 | 0.0000% | 0/8 | 0.532% | 98.138% | 81% | 0.081 | 1140 |
| 64 segs/group, 1-of-3 | 0.0000% | 0/8 | 0.213% | 98.784% | 81% | 0.051 | 1068 |
| 64 segs/group, 10-of-20 | 0.0000% | 0/8 | 0.626% | 98.297% | 81% | 0.078 | 808 |
| 256 segs/group, 1-of-3 | 0.0003% | 1/8 | 0.231% | 98.719% | 81% | 0.049 | 1625 |
| 256 segs/group, 10-of-20 | 0.0000% | 0/8 | 0.618% | 98.416% | 81% | 0.075 | 1709 |

## 6. Host loss rate (home mix)

| Case | Segments lost per year | Runs with loss | Segment unavailable | 1 GiB file readable | Mean uptime | Repair Mbps/host (800 GB held) | Peak backlog |
|---|---|---|---|---|---|---|---|
| 10%/yr, 1-of-3 | 0.0000% | 0/8 | 0.222% | 98.840% | 81% | 0.028 | 850 |
| 10%/yr, 10-of-20 | 0.0000% | 0/8 | 0.485% | 98.918% | 81% | 0.033 | 454 |
| 20%/yr, 1-of-3 | 0.0003% | 2/8 | 0.212% | 98.581% | 81% | 0.050 | 1454 |
| 20%/yr, 10-of-20 | 0.0000% | 0/8 | 0.532% | 98.138% | 81% | 0.081 | 1140 |
| 50%/yr, 1-of-3 | 0.0033% | 5/8 | 0.210% | 98.052% | 81% | 0.109 | 1765 |
| 50%/yr, 10-of-20 | 0.0000% | 0/8 | 0.472% | 97.018% | 81% | 0.210 | 2104 |
| 100%/yr, 1-of-3 | 0.0247% | 8/8 | 0.248% | 97.217% | 81% | 0.208 | 2377 |
| 100%/yr, 10-of-20 | 0.0016% | 3/8 | 0.447% | 96.244% | 81% | 0.420 | 4346 |

## 7. Does a pool actually fill? (headroom invariant)

50 users pledge 1 TB each with a 10 GB allowance and unlimited demand, at 1-of-3. 'Full' means reaching (1 - reserve) of capacity.

| reserve | headroom | headroom × (1 − reserve) | Final fill | Result |
|---|---|---|---|---|
| 0.2 | 1 | 0.80 | 3.2% | **stalls** |
| 0.2 | 1.2 | 0.96 | 15.7% | **stalls** |
| 0.2 | 1.3 | 1.04 | 79.8% | fills |
| 0.2 | 1.5 | 1.20 | 79.8% | fills |
| 0.2 | 2 | 1.60 | 79.8% | fills |
| 0.1 | 1 | 0.90 | 7.7% | **stalls** |
| 0.1 | 1.2 | 1.08 | 90.0% | fills |
| 0.3 | 1.5 | 1.05 | 69.9% | fills |

## 8. Can a full pool recover from host loss?

Pool at its fill limit loses a share of hosts at once. Spare is what remains on survivors, Needed is the lost data to re-store.

| reserve | Hosts lost | Spare | Needed | Result |
|---|---|---|---|---|
| 0.05 | 5.0% | 0.048 | 0.048 | recovers |
| 0.05 | 10.0% | 0.045 | 0.095 | **short of space** |
| 0.05 | 20.0% | 0.040 | 0.190 | **short of space** |
| 0.05 | 30.0% | 0.035 | 0.285 | **short of space** |
| 0.1 | 5.0% | 0.095 | 0.045 | recovers |
| 0.1 | 10.0% | 0.090 | 0.090 | recovers |
| 0.1 | 20.0% | 0.080 | 0.180 | **short of space** |
| 0.1 | 30.0% | 0.070 | 0.270 | **short of space** |
| 0.2 | 5.0% | 0.190 | 0.040 | recovers |
| 0.2 | 10.0% | 0.180 | 0.080 | recovers |
| 0.2 | 20.0% | 0.160 | 0.160 | recovers |
| 0.2 | 30.0% | 0.140 | 0.240 | **short of space** |
| 0.3 | 5.0% | 0.285 | 0.035 | recovers |
| 0.3 | 10.0% | 0.270 | 0.070 | recovers |
| 0.3 | 20.0% | 0.240 | 0.140 | recovers |
| 0.3 | 30.0% | 0.210 | 0.210 | recovers |

## 9. Usable space per TB contributed

| Policy | Expansion | Usable at 20% reserve | Usable at 10% reserve |
|---|---|---|---|
| 1-of-3 | 3.0x | 267 GB | 300 GB |
| 1-of-5 | 5.0x | 160 GB | 180 GB |
| 6-of-12 | 2.0x | 400 GB | 450 GB |
| 10-of-16 | 1.6x | 500 GB | 562 GB |
| 10-of-20 | 2.0x | 400 GB | 450 GB |

## 10. Attacks

**Sybil share of segments, using real placement** (1,000 hosts, 100,000 placements)

| Attacker hosts | All replicas of 1-of-3 | Analytic f³ | More than 10 of 20 pieces (enough to destroy a 10-of-20 segment) |
|---|---|---|---|
| 1.0% | 0.0000% | 0.0001% | 0.0000% |
| 10.0% | 0.0990% | 0.1000% | 0.0000% |
| 33.0% | 3.6440% | 3.5937% | 3.3820% |

**Sybil using one site for its identities.** With the per-site rule, a single-site attacker holds at most one piece of any segment regardless of identity count. The prototype enforces this in the public pool and tests it.

**False pledge.** Attacker pledges 10 TB and really has 1 TB, with reserve 20% and headroom 1.5. Credit is bounded by proof:

| Really holding | Credit | Raw quota | Note |
|---|---|---|---|
| 0 GB | 10 GB | 8 GB | honest so far |
| 100 GB | 160 GB | 128 GB | honest so far |
| 500 GB | 760 GB | 608 GB | honest so far |
| 1000 GB | 1510 GB | 1208 GB | next placement is refused, and the lie surfaces |

The attacker's gain is `1.2 × held + 8 GB` raw while holding `held`, compared with `0.8 × held` for an honest participant with a true pledge at capacity. At the moment the disk fills, a liar has 1,208 GB raw against an honest 800 GB, an overdraft of about 51%. Once placements start failing, credit falls back and the lifecycle moves them to read-only. Lower `headroom` reduces this, but section 7 shows it can't go below about 1.3 at 20% reserve.

**Hit and run.** A leaver's data lasts through grace and read-only (44 days provisional). The network pays one repair of the bytes they held, and they keep nothing. At 20%/yr honest loss, this is ordinary churn unless many do it at once.

**Identical replicas.** An operator holding several copies of a segment under different identities needs to keep only one. This is an accepted risk at `k=1`, and erasure-coded pieces aren't affected. This table gives how much disk the attack saves.

| Attacker share f | Expected pieces per segment on attacker | Segments with at least one | Attacker's disk saved |
|---|---|---|---|
| 1.0% | 0.030 | 3.0% | 1.0% |
| 10.0% | 0.300 | 27.1% | 9.7% |
| 33.0% | 0.990 | 69.9% | 29.4% |

The saving is roughly `f`: whenever an attacker holds two or more copies, it needs to keep just one. It also collapses the real number of independent copies, so the durability harm exceeds the disk saving.

## 11. The bandwidth rule

Download limited to measured upload (with no banked credit), and 1 TB of logical data at 1-of-3.

| Link | Upload time (3 TB) | Restore 1 TB at entitled rate | Restore with banked credit |
|---|---|---|---|
| 500/20 | 13.9 d | 4.6 d | 0.2 d |
| 100/20 | 13.9 d | 4.6 d | 0.9 d |
| 80/20 | 13.9 d | 4.6 d | 1.2 d |
| 1000/100 | 2.8 d | 0.9 d | 0.1 d |
| 1000/1000 | 0.3 d | 0.1 d | 0.1 d |
| 50/10 | 27.8 d | 9.3 d | 2.1 d |

The last column assumes 90 days of serving at 10% of upload banked, and spending it at the link's download speed.

**Everyone restores at once** (regional disaster): sustained demand can't exceed supply, because each reader's entitled rate equals what they offer. That argument assumes accurate measurement, and it ignores repair traffic, offline hosts and banked bursts, all of which can make real supply fall short for a while. It isn't tested by this simulator.

