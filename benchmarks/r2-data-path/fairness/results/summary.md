| RTT ms | buffer BDP | flows | Jain | per flow: goodput share / loss / CPU | queue mean |
|---|---|---|---|---|---|
| 20 | 0.5 | bbr | 1.000 | bbr 100.0% / loss 67.6% / 14.6 ms/MiB | 96% |
| 20 | 0.5 | bbr + cubic | 0.507 | bbr 99.3% / loss 67.9% / 14.1 ms/MiB ; cubic 0.7% / loss 24.4% / - ms/MiB | 97% |
| 20 | 0.5 | bbr + tcp | 0.505 | bbr 99.5% / loss 64.9% / 13.5 ms/MiB ; tcp 0.5% / loss 21.4% / - ms/MiB | 97% |
| 20 | 0.5 | cubic | 1.000 | cubic 100.0% / loss 0.0% / 7.8 ms/MiB | 42% |
| 20 | 0.5 | cubic + tcp | 0.958 | cubic 39.6% / loss 0.1% / 9.1 ms/MiB ; tcp 60.4% / loss 0.2% / 0.9 ms/MiB | 53% |
| 20 | 0.5 | tcp | 1.000 | tcp 100.0% / loss 0.4% / 0.8 ms/MiB | 33% |
| 20 | 1.0 | bbr | 1.000 | bbr 100.0% / loss 70.0% / 17.3 ms/MiB | 95% |
| 20 | 1.0 | bbr + cubic | 0.506 | bbr 99.5% / loss 68.7% / 15.3 ms/MiB ; cubic 0.5% / loss 24.5% / - ms/MiB | 96% |
| 20 | 1.0 | bbr + tcp | 0.504 | bbr 99.6% / loss 68.2% / 20.4 ms/MiB ; tcp 0.4% / loss 25.3% / - ms/MiB | 97% |
| 20 | 1.0 | cubic | 1.000 | cubic 100.0% / loss 0.1% / 8.8 ms/MiB | 61% |
| 20 | 1.0 | cubic + tcp | 0.955 | cubic 39.2% / loss 0.0% / 13.9 ms/MiB ; tcp 60.8% / loss 0.1% / 1.1 ms/MiB | 70% |
| 20 | 1.0 | tcp | 1.000 | tcp 100.0% / loss 0.3% / 0.9 ms/MiB | 38% |
| 20 | 4.0 | bbr | 1.000 | bbr 100.0% / loss 62.0% / 12.6 ms/MiB | 88% |
| 20 | 4.0 | bbr + cubic | 0.505 | bbr 99.5% / loss 62.3% / 13.0 ms/MiB ; cubic 0.5% / loss 12.0% / - ms/MiB | 88% |
| 20 | 4.0 | bbr + tcp | 0.504 | bbr 99.7% / loss 61.3% / 12.2 ms/MiB ; tcp 0.4% / loss 13.1% / - ms/MiB | 89% |
| 20 | 4.0 | cubic | 1.000 | cubic 100.0% / loss -0.1% / 7.5 ms/MiB | 56% |
| 20 | 4.0 | cubic + tcp | 0.980 | cubic 53.8% / loss -0.0% / 8.4 ms/MiB ; tcp 46.2% / loss 0.0% / 0.9 ms/MiB | 85% |
| 20 | 4.0 | tcp | 1.000 | tcp 100.0% / loss 0.1% / 0.8 ms/MiB | 68% |
| 80 | 0.5 | bbr | 1.000 | bbr 100.0% / loss 63.6% / 12.8 ms/MiB | 84% |
| 80 | 0.5 | bbr + cubic | 0.504 | bbr 99.6% / loss 64.1% / 13.0 ms/MiB ; cubic 0.4% / loss 10.9% / - ms/MiB | 84% |
| 80 | 0.5 | bbr + tcp | 0.503 | bbr 99.7% / loss 63.5% / 13.0 ms/MiB ; tcp 0.3% / loss 16.7% / - ms/MiB | 82% |
| 80 | 0.5 | cubic | 1.000 | cubic 100.0% / loss -0.0% / 7.1 ms/MiB | 21% |
| 80 | 0.5 | cubic + tcp | 0.937 | cubic 37.0% / loss 0.0% / 8.9 ms/MiB ; tcp 63.0% / loss 0.4% / 0.8 ms/MiB | 8% |
| 80 | 0.5 | tcp | 1.000 | tcp 100.0% / loss 0.5% / 0.8 ms/MiB | 1% |
| 80 | 1.0 | bbr | 1.000 | bbr 100.0% / loss 57.1% / 11.5 ms/MiB | 79% |
| 80 | 1.0 | bbr + cubic | 0.504 | bbr 99.6% / loss 57.1% / 12.0 ms/MiB ; cubic 0.4% / loss 8.8% / - ms/MiB | 78% |
| 80 | 1.0 | bbr + tcp | 0.503 | bbr 99.7% / loss 57.3% / 11.5 ms/MiB ; tcp 0.3% / loss 12.2% / - ms/MiB | 78% |
| 80 | 1.0 | cubic | 1.000 | cubic 100.0% / loss -0.3% / 11.2 ms/MiB | 20% |
| 80 | 1.0 | cubic + tcp | 0.964 | cubic 40.3% / loss -0.2% / 8.3 ms/MiB ; tcp 59.7% / loss 0.1% / 0.8 ms/MiB | 67% |
| 80 | 1.0 | tcp | 1.000 | tcp 100.0% / loss 0.1% / 0.9 ms/MiB | 2% |
| 80 | 4.0 | bbr | 1.000 | bbr 100.0% / loss 27.9% / 10.1 ms/MiB | 72% |
| 80 | 4.0 | bbr + cubic | 0.506 | bbr 99.4% / loss 28.8% / 11.2 ms/MiB ; cubic 0.6% / loss 3.6% / - ms/MiB | 71% |
| 80 | 4.0 | bbr + tcp | 0.505 | bbr 99.5% / loss 28.9% / 11.1 ms/MiB ; tcp 0.5% / loss 3.9% / - ms/MiB | 72% |
| 80 | 4.0 | cubic | 1.000 | cubic 100.0% / loss -0.6% / 12.1 ms/MiB | 48% |
| 80 | 4.0 | cubic + tcp | 0.900 | cubic 60.2% / loss -0.2% / 9.1 ms/MiB ; tcp 39.8% / loss 0.0% / 0.9 ms/MiB | 74% |
| 80 | 4.0 | tcp | 1.000 | tcp 100.0% / loss 0.0% / 0.9 ms/MiB | 56% |
