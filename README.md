# Botka

Telegram bot backend with async handlers, SQLAlchemy, and Dishka DI.

## Requirements
- Python 3.12+
- uv
- Docker (optional)

## Setup
1) Copy env file:

```
cp .env.example .env
```

2) Edit `.env` with your bot token and database URL (SQLite by default).

## Run locally with uv
```
uv sync
uv run botka
```

## Run with Docker Compose
```
docker compose up --build
```

## 3D printers

`/3d` shows status and camera buttons for configured Bambu Lab and Klipper
printers. Bambu printers use `BOTKA_BAMBU_PRINTERS`. Klipper printers use the
Moonraker API and are configured with `BOTKA_KLIPPER_PRINTERS`:

```env
BOTKA_KLIPPER_PRINTERS='[{"name":"Voron","base_url":"http://voron.local","api_key":"","camera_url":"/webcam/?action=snapshot"}]'
```

`api_key` and `camera_url` are optional. If `camera_url` is omitted, the bot
uses the first enabled webcam reported by Moonraker's `/server/webcams/list`.
Printer names must be unique across both integrations.
