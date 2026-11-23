FROM rust:1-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p stringcan-api

FROM debian:stable-slim
RUN useradd --system --no-create-home stringcan
COPY --from=build /src/target/release/stringcan-api /usr/local/bin/stringcan-api
USER stringcan
ENV BIND_ADDR=0.0.0.0:8080
EXPOSE 8080
CMD ["stringcan-api"]
