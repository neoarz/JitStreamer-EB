build:
  cargo build --release
run: build
  sudo ./target/release/jitstreamer-eb
docker-build:
  sudo docker build -t jitstreamer-eb .
docker-run:
  sudo docker run --rm -it \
  -p 9172:9172 \
  -p 51869:51869 \
  -v jitstreamer-lockdown:/var/lib/lockdown \
  -v $(pwd)/jitstreamer.db:/app/jitstreamer.db \
  jitstreamer-eb
