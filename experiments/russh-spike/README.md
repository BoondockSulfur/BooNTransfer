# russh-spike — Phase-1-Prototyp & Benchmark

Isolierter Vergleich **russh + russh-sftp** vs. **ssh2 (libssh2)** für SFTP —
ohne den App-Code anzufassen. Zweck: den realen Tempogewinn *messen*, bevor der
stabile SSH-Kern umgebaut wird.

## Ergebnis (Docker-sshd, netem-Latenz auf Server-Antworten)

Download-Durchsatz — drei Backends: **ssh2 (heute)**, **russh seriell**,
**russh pipelined** (selbst gebaut über `RawSftpSession`, 64 parallele Requests):

| Latenz | ssh2 | russh seriell | **russh pipelined** |
|---|---|---|---|
| ~0 ms | 197 MB/s | 99 MB/s | **243 MB/s** |
| 25 ms | ~28 MB/s | ~8 MB/s | **~27 MB/s** |
| 50 ms | ~18–20 MB/s | ~4.5 MB/s | ~13 MB/s |

Uploads: **pipelined russh ≈ oder ≥ ssh2 bei allen Latenzen** (0 ms: 125 vs 99).

**Kernbefunde:**
1. **Serielles russh-sftp ist langsam** (kein Read-Ahead) → 2–3× unter ssh2.
2. **Selbst gebautes Pipelining dreht das um:** von ~8 auf ~27 MB/s bei 25 ms —
   **schneller als ssh2 bei 0 ms, gleichauf bis ~25 ms RTT.**
3. **Upload** ist mit Pipelining bei allen Latenzen mindestens gleichauf.
4. Bei **50 ms** liegt die *naive* Pipeline noch unter libssh2s ausgereiftem
   Read-Ahead (Feintuning: Chunk-Größe/Concurrency/Timeout/Fenster; netem-Messung
   verrauscht). **Kein grundsätzlicher Blocker** — russh-sftps async
   `RawSftpSession` erlaubt volles Pipelining; die 50-ms-Feinabstimmung ist eine
   begrenzte Ausbauaufgabe. Blockierendes ssh2 kann Pipelining prinzipiell **nicht**.

**Fazit:** Für den realistischen Fall (naher Server, ~5–30 ms RTT, z. B. Hetzner)
ist **russh mit Pipelining so schnell wie/schneller als ssh2** — plus der
pure-Rust-Vorteil (kein libssh2/OpenSSL). Basis für BooN-OS tragfähig.

## Reproduzieren

```bash
# 1) SSH-Server-Container starten
docker run -d --name russh-bench --cap-add NET_ADMIN \
  -e PUID=1000 -e PGID=1000 -e PASSWORD_ACCESS=true \
  -e USER_NAME=tester -e USER_PASSWORD=testpass -e SUDO_ACCESS=false \
  -p 2222:2222 lscr.io/linuxserver/openssh-server:latest

# 2) Baseline (localhost)
cargo run --release -- both 127.0.0.1 2222 tester testpass /config 32

# 3) künstliche Latenz und erneut messen
docker exec russh-bench sh -c 'apk add --no-cache iproute2'
docker exec russh-bench tc qdisc replace dev eth0 root netem delay 25ms
cargo run --release -- both 127.0.0.1 2222 tester testpass /config 8

# gegen einen ECHTEN Server (z. B. Hetzner):
cargo run --release -- both <host> 22 <user> <pass> <schreibbarer_remote_dir> 32

# aufräumen
docker rm -f russh-bench
```

## Aufruf
`russh-spike <russh|ssh2|both> <host> <port> <user> <pass> <remote_dir> <size_mb>`

> Nur Passwort-Auth im Prototyp; Host-Key wird akzeptiert (kein known_hosts).
> Nicht für Produktion — reines Messwerkzeug.
