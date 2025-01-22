# Jackson Coxson
# todo finish this

# Use the official Rust image as a base
FROM rust:latest

# Set environment variables
ENV CARGO_HOME=/usr/local/cargo
ENV PATH=$CARGO_HOME/bin:$PATH

# Install necessary tools and dependencies
RUN apt-get update && apt-get install -y \
    git \
    python3 \
    python3-pip \
    wireguard \
    && rm -rf /var/lib/apt/lists/*

# Install pymobiledevice3
RUN pip3 install pymobiledevice3

# Set the working directory
WORKDIR /usr/src/netmuxd

# Clone the netmuxd repository
RUN git clone https://github.com/jkcoxson/netmuxd.git .

# Build the project
RUN cargo build --release

# Expose the port that netmuxd will use (adjust if necessary)
EXPOSE 8080
VOLUME /data

# Set the command to run the built binary
CMD ["./target/release/netmuxd"]

