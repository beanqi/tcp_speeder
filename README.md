
## Get and Compile the Source Code

```bash
git clone https://github.com/beanqi/tcp_speeder.git

cargo build --release
```

## Copy the Executable to the Server

Place the compiled executable file, `tcp_speeder`, into the `/opt/tcp_speeder` directory.

## Edit the Service File

Create the file: `/etc/systemd/system/tcp-speeder.service`

Copy the following content:

```
[Unit]
Description=TCP Speeder Service
After=network.target

[Service]
Type=simple
WorkingDirectory=/opt/tcp_speeder
ExecStart=/opt/tcp_speeder/tcp_speeder
User=root
Group=root
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Execute the following commands to start the service and enable it to start on boot:

```bash
sudo systemctl daemon-reload
sudo systemctl enable tcp-speeder.service
sudo systemctl start tcp-speeder.service
```

### Test Exchange HTTP Latency

Kraken:

```bash
curl -L -o /dev/null -s -w "%{time_namelookup}:%{time_connect}:%{time_appconnect}:%{time_pretransfer}:%{time_starttransfer}:%{time_total}" 'https://api.kraken.com/0/public/Time' -H 'Accept: application/json' | awk -F: '{
    printf "DNS lookup time: %.0fms\n", $1 * 1000;
    printf "TCP connection time: %.0fms\n", $2 * 1000;
    printf "SSL handshake time: %.0fms\n", $3 * 1000;
    printf "Pre-transfer time: %.0fms\n", $4 * 1000;
    printf "Start transfer time: %.0fms\n", $5 * 1000;
    printf "Total time: %.0fms\n", $6 * 1000;
}'
```

BtcTurk:

```bash
curl -L -o /dev/null -s -w "%{time_namelookup}:%{time_connect}:%{time_appconnect}:%{time_pretransfer}:%{time_starttransfer}:%{time_total}" 'https://api.btcturk.com/api/v2/trades?pairSymbol=BTCUSDTF' -H 'Accept: application/json' | awk -F: '{
    printf "DNS lookup time: %.0fms\n", $1 * 1000;
    printf "TCP connection time: %.0fms\n", $2 * 1000;
    printf "SSL handshake time: %.0fms\n", $3 * 1000;
    printf "Pre-transfer time: %.0fms\n", $4 * 1000;
    printf "Start transfer time: %.0fms\n", $5 * 1000;
    printf "Total time: %.0fms\n", $6 * 1000;
}'
```
