# 09 — Cost

Prices are as of 2026-10-29. Each is an input, not a commitment: re-check it before a
decision. Every derived figure is recomputable from the Inputs table, with its formula.

## The unit

One container-month at `standard-4`: 4 vCPU, 12 GiB, 20 GB of ephemeral disk, 730 hours — the
Cloudflare Containers instance ceiling, and the common denominator for all three prices here.

## Inputs

| Input | Value | Source |
|---|---|---|
| Cloudflare Containers, vCPU | $0.000020 / vCPU-s | Cloudflare Containers pricing, 2026-10-29 |
| Cloudflare Containers, memory | $0.0000025 / GiB-s | Cloudflare Containers pricing, 2026-10-29 |
| Cloudflare Containers, disk | $0.00000007 / GB-s | Cloudflare Containers pricing, 2026-10-29 |
| AWS Fargate, vCPU | $0.000011244 / vCPU-s | AWS Fargate pricing, 2026-10-29 |
| AWS Fargate, memory | $0.000001235 / GiB-s | AWS Fargate pricing, 2026-10-29 |
| Node hardware | $40,520 per node, 48-month life | refurbished 2× EPYC 9534, 128 cores, 256 GiB, 8× 1.92 TB |
| Node power draw | 0.55 kW | node power budget |
| PUE | 1.54 | Uptime Institute 2025 |
| Electricity | $0.1447 / kWh | EIA Electric Power Monthly, Aug 2026 |
| Colo | $200 / kW-month | Digital Realty / CBRE via ColocationScout |
| Transit | $50 / month per node | TeleGeography Q2 2026, 1 Gbps commit |
| Loaded labour | $128,869 / year per person; 0.5 FTE allocated | BLS OEWS May 2025 and ECEC Jun 2026 |
| Nodes per ops person | 109.2 (0.5 FTE covers 54.6 nodes) | cost model assumption |
| Monitoring, backups, spares | $169 / node-month | budget line |
| R2 storage | $0.015 / GB-month | Cloudflare R2 pricing, 2026-10-29 |
| Storage node | $700 / month, 240 TB raw | 12 × 20 TB, fleet table below |

## Metered container-month

Cloudflare and Fargate bill per second for vCPU and memory; Cloudflare also bills for disk.

```
Cloudflare = (4×0.000020 + 12×0.0000025 + 20×0.00000007) × 3600 × 730
           = 0.0001114 × 3600 × 730 = $292.76

Fargate    = (4×0.000011244 + 12×0.000001235) × 3600 × 730
           = 0.0000597960 × 3600 × 730 = $157.14
```

## Owned node-month

One node is 2× EPYC 9534 (128 cores), 256 GiB, 8× 1.92 TB, carried at 100% duty.

| Line | Formula | Monthly |
|---|---|---|
| Hardware | $40,520 / 48 months | $844.17 |
| Power | 0.55 kW × 730 h × 1.54 PUE × $0.1447/kWh | $89.47 |
| Colo | 0.55 kW × $200/kW-month | $110.00 |
| Transit | 1 Gbps commit | $50.00 |
| Labour | $128,869 / 12 / 109.2 nodes | $98.34 |
| Monitoring, backups, spares | budget line | $169.00 |
| **Node-month** | sum of the lines above | **$1,360.98** |

Container-equivalents per node are set by whichever resource binds first:

```
min(128 vCPU / 4, 256 GiB / 12) = min(32, 21.33) = 21.33   (memory-bound)

owned container-month = 1360.98 / (256/12) = $63.80
```

## Break-even

```
owned / Cloudflare = 63.80 / 292.76 = 21.8%
owned / Fargate    = 63.80 / 157.14 = 40.6%
```

Owned metal is cheaper than a metered container only above those duty cycles, because the node
is paid for whether it runs or not. A CI fleet at 8 h × 22 weekdays is 176 h of 730, or 24.1%
duty, where the monthly compute line is $37.89 on Fargate, $70.58 on Cloudflare Containers and
$63.80 for the amortised node: 1.7× the Fargate cost and 0.9× the Cloudflare cost.

## Labour sensitivity

Labour per node-month is `128869 × 0.5 / 12 / nodes`; the owned container-month is
`(1262.64 + labour) / 21.33`, where $1,262.64 is the node-month less labour.

| Nodes per ops person | Labour $/node-month | Node-month | Owned $/container-month | Break-even vs Fargate |
|---|---|---|---|---|
| 109.2 | 98.3 | 1,361.0 | 63.8 | 40.6% |
| 54.6 | 196.7 | 1,459.3 | 68.4 | 43.5% |
| 20 | 537.0 | 1,799.6 | 84.4 | 53.7% |
| 10 | 1,073.9 | 2,336.5 | 109.5 | 69.7% |
| 2 | 5,369.5 | 6,632.2 | 310.9 | 197.9% |

## Where owned metal is cheaper

| Workload class | Margin | Reason |
|---|---|---|
| Object storage above ~80 TB | 1.5× at 80 TB, 3.2× at 168 TB | R2 is $15/TB-month. One 240 TB storage node at 70% usable holds 168 TB for $700/month plus a $100/month share of site power and colo, or $4.76/TB-month. Crossover is 53 TB. |
| Sustained compute above 40.6% duty | 2.5× at 100% duty | $157.14 on Fargate against $63.80 owned; against Cloudflare's $292.76, 4.6×. |
| Bursty compute below 40.6% duty | 0.6× | At 24.1% duty Fargate bills $37.89 against $63.80 for metal idle 76% of the month. |

## The realistic fleet

3 CI nodes, 1 storage node, 2 gateways.

| Item | Monthly |
|---|---|
| 3 × CI node (16c / 64 GiB / 2 TB NVMe) | $1,800 |
| 1 × storage node (12 × 20 TB) | $700 |
| 2 × gateway (small) | $150 |
| Colo, power, transit | $600 |
| Cloudflare Workers Paid | $5 |
| R2 for logs | $10 |
| Labour (0.5 FTE) | $5,400 |
| **Total** | **$8,665** |

The sum is $8,665, or ~$8,700 rounded; labour is $5,400 of it, 62.3%. Infrastructure, power,
colo and the two metered Cloudflare lines are the other 37.7%. The 0.5 FTE here is the minimum
cover for a six-node fleet, not the 109.2-node ratio the unit cost assumes: a fleet this small
needs a second person on call whether or not the arithmetic divides that finely.

## Assumptions and sensitivity

| Assumption | Base | Change | Effect |
|---|---|---|---|
| Hardware life | 48 months | 36 months | hardware $844.17 → $1,125.56 (+33.3%); container-month $63.80 → $77.00; break-even vs Fargate 40.6% → 49.0% |
| Loaded labour | $128,869/yr | $180,000/yr | labour $98.34 → $137.36; container-month $63.80 → $65.62; break-even vs Fargate 41.8% |
| Nodes per ops person | 109.2 | 20 nodes per person | container-month $63.80 → $84.36; break-even 53.7% |
| PUE | 1.54 | ±0.3 | power $89.47 ± $17.43; container-month ± $0.82 |
| Electricity | $0.1447/kWh | $0.0964/kWh industrial | power $89.47 → $59.61; container-month $63.80 → $62.40 |
| Colo | $200/kW-month | $190–293/kW-month | colo $104.50–$161.15; container-month $63.54–$66.19 |

Hardware life moves the container-month most among the infrastructure inputs, at +$13.19;
electricity, PUE and colo move it by at most $2.40.

## The term that moves the result

Labour is the input the result turns on. At 109.2 nodes per person it is $98.34 of a $1,360.98
node-month; at 10 nodes per person it is $1,073.91 of $2,336.55, and the break-even against
Fargate moves from 40.6% to 69.7% — $46 of container-month between those two points. No
infrastructure variation tested moves the container-month by more than $14.

Hardware is the largest single line at a favourable ratio, $844.17 of $1,360.98, and it is the
least elastic: a 36-month life raises the container-month by $13.19. The agent design acts on
the ratio rather than on the hardware — one static binary per host, holding the connection,
supervision, metrics and the local API, replaces a set of daemons that each carry their own
configuration, credentials, upgrade path and failure mode. Fewer components per node is the
same statement as more nodes per person, and that ratio is what the break-even moves with.
