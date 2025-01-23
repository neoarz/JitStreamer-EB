# JitStreamer EB

The sequel that nobody wanted, but everyone needed.

JitStreamer is a program to activate JIT across the far reaches of the internet.

I authored the original JitStreamer a few years ago, but Apple has since changed
how the protocol for debugging apps works. This program is a rewrite of that original
program, while using the new protocol.

Simply put, this program takes a pairing file and returns a Wireguard configuration.
That Wireguard configuration allows the device to interact with a server that will
activate JIT on the device.

## EB

What is EB? Electric Boogaloo.
[r/outoftheloop](https://www.reddit.com/r/OutOfTheLoop/comments/3o41fi/where_does_the_name_of_something2_electric/)

## Building

```bash
cargo build --release

```

It's not that deep.

## Running

1. Start [netmuxd](https://github.com/jkcoxson/netmuxd)
2. Install the pip requirements

```bash
pip install -r requirements.txt
```

3. Start tunneld

```bash
sudo python3 -m pymobildevice3 remote tunneld
```

4. Run the program

```bash
./target/release/jitstreamer-eb
```

**OR**

```bash
just run
```

5. Start the Wireguard peer

```bash
sudo wg-quick up jitstreamer
```

6. ???
7. Profit

## Docker

There's a nice dockerfile that contains a Wireguard server and JitStreamer server,
all packaged and ready to go. It contains everything you need to run the server.

```bash
just docker-build
just docker-run
```

## License

MIT

## Contributing

Please do. Pull requests will be accepted after passing cargo clippy.

## Thanks

- [ny](https://github.com/nythepegasus/SideJITServer) for the Python implementation
- [pymobiledevice3](https://github.com/doronz88/pymobiledevice3)
