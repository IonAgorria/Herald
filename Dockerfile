#Create base runtime stage
FROM debian:stable-slim as runtime

#Install runtime dependencies
RUN apt-get update
RUN apt-get install -y ca-certificates
RUN rm -rf /var/lib/apt/lists/*

#Fixes "panicked at 'no CA certificates found'" RustTLS error
RUN update-ca-certificates

FROM rust:latest as builder

WORKDIR /usr/src/app

#Copy stuff
COPY Cargo.* ./
COPY src/ ./src/
COPY serde_kdlab_xprm/ ./serde_kdlab_xprm/

#Compile
RUN cargo install --path .

#Add binaries to runtime stage
FROM runtime

#Copy runtime stuff
COPY --from=builder /usr/local/cargo/bin/herald /usr/local/bin/herald
COPY docker_entry.sh/ /usr/src/app/

#Setup user
RUN groupadd -f -g 999 user
RUN useradd -r -m -d /home/user -s /bin/bin/nologin -u 999 -g user user

RUN mkdir /data
WORKDIR /data

ENTRYPOINT ["/usr/src/app/docker_entry.sh"]
CMD ["herald"]
