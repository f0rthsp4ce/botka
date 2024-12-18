# F0RTHSP4CE Telegram Bot

<p>
<a href="https://t.me/F0RTHSP4CE_bot"><img alt="Telegram Bot" src="https://img.shields.io/badge/Telegram-%40F0RTHSP4CE__bot-blue?logo=telegram"></a>
<a href="https://t.me/c/1900643629/7882"><img alt="Internal Discussion Topic" src="https://img.shields.io/badge/Internal_Discussion_Topic-Internal_issue_bot-blue?logo=data%3Aimage%2Fgif%3Bbase64%2CR0lGODlhEAAQAPQBAAAAAAEBASoUBQENN0sLA28TA05REn9%2BGwEVUQIaY4kYBLJICX6CGYuKHZ2cIbmaKamqIv%2BhHtTSK%2FP0MgIkijMviAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAACH5BAEAAAAALAAAAAAQABAAAAVWICCOQRmMKGmuKRAka3ySAwXLs2siFIKLsgRFppPVBrhkJRkrLCIPA7OgCDQmDImgVCQoCAEDRCJxcHWFQkkwmUhNIwHYdJ00pCicQzI55FRELYB%2FIiEAOw%3D%3D"></a>
<a href="https://wiki.f0rth.space/en/residents/telegram-bot"><img alt="Wiki" src="https://img.shields.io/badge/Wiki-Project_Page-blue?logo=wikidotjs"></a>
<a href="http://10.0.24.18:42777"><img alt="HTTP API" src="https://img.shields.io/badge/HTTP_API-10.0.24.18%3A42777-blue?logo=openapiinitiative"></a>
<a href="https://grafana.lo.f0rth.space/d/cbdbf909-7f4d-409b-9e6d-07dff89b3a10/botka"><img alt="Grafana Dashboard" src="https://img.shields.io/badge/Grafana_Dashboard-Botka-blue?logo=grafana"></a>
<img alt "License: Unlicense OR MIT" src="https://img.shields.io/badge/License-Unlicense%20OR%20MIT-blue?logo=unlicense">
</p>

## Build

This project uses Nix flakes to manage dependencies, ensuring a reliable and reproducible build environment. To get started:

1. Install Nix or NixOS by following instructions at [nixos.org/download](https://nixos.org/download).
2. Enable Nix flakes as per the guide on [NixOS Wiki](https://nixos.wiki/wiki/Flakes#Enable_flakes).

Alternatively, install `Cargo` and `Rust` using the instructions found at [Rust's official site](https://doc.rust-lang.org/cargo/getting-started/installation.html).

To build the project:

- For a release build, run `nix build`. The resulting binary can be found at `./result/bin/f0bot`.
- For setting up a development environment with necessary dependencies, run `nix develop`. Inside this environment, you can compile the project with `cargo build`.

## Running the Bot Locally

1. Use [@BotFather](https://t.me/BotFather) to create a new Telegram bot, create a test chat with topics, and add the bot as an administrator.
2. Copy [`config.example.yaml`](./config.example.yaml) and adjust it as needed, particularly the `telegram.token`.
3. Start the bot with `cargo run bot my-config.yaml`.

## Development Conventions

This project follows these conventions:

- **Code Style and Lints**: Refer to the [`./Justfile`](./Justfile).
- **Commit Messages**: [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/).
- **Metric Naming**: [Prometheus metric and label naming guidelines](https://prometheus.io/docs/practices/naming/).
