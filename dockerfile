# Jackson Coxson
# todo finish this

# Use a base image with Rust for building the project
FROM rust:latest AS builder

# Set the working directory
WORKDIR /app

# Copy the project files into the container
COPY . .

# Build the JitStreamer EB project in release mode
RUN cargo build --release

# Prepare the final runtime image
FROM debian:bookworm-slim

# Install required runtime dependencies
RUN apt-get update && apt-get install -y \
    wireguard-tools \
    iproute2 \
    librust-openssl-dev \
    libssl-dev && \
    rm -rf /var/lib/apt/lists/*

# Copy the built binary and necessary files from the builder stage
COPY --from=builder /app/target/release/jitstreamer-eb /usr/local/bin/jitstreamer-eb

# Set the default working directory
WORKDIR /app
RUN mkdir -p /var/lib/lockdown
RUN mkdir -p /etc/wireguard

# Expose Wireguard and Jitstreamer ports
EXPOSE 51869/udp
EXPOSE 9172/tcp

VOLUME /var/lib/lockdown
VOLUME /etc/wireguard
VOLUME /app/jitstreamer.db

# Command to start all required services and run the program
CMD ["/bin/bash", "-c", "wg-quick up jitstreamer & jitstreamer-eb"]
