from __future__ import annotations

import json
from typing import Iterable

from pydantic import Field, field_validator
from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    bot_token: str
    database_url: str

    # Planka integration
    planka_base_url: str | None = None
    planka_username_or_email: str | None = None
    planka_password: str | None = None
    planka_card_type: str = "project"
    planka_todo_list_id: str | None = None
    planka_doing_list_id: str | None = None
    planka_done_list_id: str | None = None
    planka_request_timeout_seconds: float = 10.0
    planka_notification_chat_ids: str | None = Field(
        default=None,
        description="Comma-separated: chat_id or chat_id:thread_id for topics",
    )
    planka_board_id: str | None = None
    planka_board_name: str = "TASKS"
    planka_poll_interval_seconds: float = 5.0
    planka_notify_new_quests: bool = True
    planka_show_card_links: bool = True
    shopping_chat_id: int | None = None
    shopping_topic_id: int | None = None
    borrowed_chat_id: int | None = None
    borrowed_topic_id: int | None = None
    pins_chat_id: int | None = None
    pins_tracked_chat_ids: list[int] | str = Field(default_factory=list)
    bootstrap_resident_ids: list[int] | str = Field(default_factory=list)
    allowed_anon_group_ids: list[int] | str = Field(default_factory=list)
    usbutler_base_url: str | None = None
    usbutler_token: str | None = None
    usbutler_timeout_seconds: float = 5.0
    gemini_api_key: str | None = None
    gemini_model: str = "gemini-2.0-flash"
    gemini_timeout_seconds: float = 10.0
    mac_tracker_base_url: str | None = None
    mac_tracker_bind_host: str = "0.0.0.0"
    mac_tracker_bind_port: int = 1818
    mac_tracker_poll_seconds: float = 30.0
    mac_tracker_jwt_secret: str | None = None
    mac_tracker_jwt_ttl_seconds: int = 900
    mac_tracker_max_last_seen_seconds: float | None = 600
    mac_tracker_allowed_subnets: list[str] | str = Field(default_factory=list)
    mac_tracker_subnet_warning_text: str | None = None
    mac_tracker_leave_grace_seconds: float = 600.0
    mac_tracker_notify_chat_id: int | None = None
    mac_tracker_notify_topic_id: int | None = None
    decisions_chat_id: int | None = None
    decisions_topic_id: int | None = None
    meeting_chat_id: int | None = None
    meeting_topic_id: int | None = None
    meeting_agenda_day: str = "tue"
    meeting_agenda_hour: int = 18
    heartbeat_chat_id: int | None = None
    heartbeat_topic_id: int | None = None
    good_morning_chat_id: int | None = None
    good_morning_topic_id: int | None = None
    good_morning_city: str | None = None
    good_morning_photo_urls: list[str] | str = Field(default_factory=list)
    periodic_heartbeat_seconds: int = 3600
    polls_maintenance_interval_seconds: int = 3600
    polls_default_close_hours: int = 168
    timezone: str | None = None
    mikrotik_base_url: str | None = None
    mikrotik_username: str | None = None
    mikrotik_password: str | None = None
    mikrotik_timeout_seconds: float = 5.0
    mikrotik_verify_tls: bool = False

    # UPS (acidmaid) integration
    ups_base_url: str | None = "http://acidmaid.local"
    ups_timeout_seconds: float = 5.0
    ups_report_chat_id: int | None = None
    ups_report_topic_id: int | None = None
    ups_check_interval_seconds: int = 30
    ups_report_interval_seconds: int = 600

    # Fridge POS integration
    fridge_pos_url: str | None = None
    fridge_pos_secret: str | None = None

    # Bambu Lab printer integration (LAN mode / access-code auth)
    # JSON list: [{"name":"X1C","ip":"192.168.1.x","serial":"...","access_code":"..."}]
    bambu_printers: str | None = None
    bambu_camera_timeout_seconds: float = 10.0

    # Klipper printer integration (Moonraker HTTP API)
    # JSON list: [{"name":"Voron","base_url":"http://voron.local","api_key":"..."}]
    klipper_printers: str | None = None
    klipper_timeout_seconds: float = 10.0

    # Visit request link (shown to guests in the menu)
    visit_request_chat_id: str | None = None  # numeric ID or @username / plain username
    visit_request_topic_id: int | None = None

    # Refinance integration
    refinance_api_url: str | None = None
    refinance_secret_key: str | None = None
    refinance_bot_entity_id: int | None = None
    refinance_invoice_poll_seconds: int = 600

    def get_bambu_printer_configs(self) -> list[dict]:
        """Parse BOTKA_BAMBU_PRINTERS JSON into a list of printer config dicts."""
        if not self.bambu_printers:
            return []
        try:
            return json.loads(self.bambu_printers)
        except (json.JSONDecodeError, TypeError):
            return []

    def get_klipper_printer_configs(self) -> list[dict]:
        """Parse BOTKA_KLIPPER_PRINTERS JSON into printer config dicts."""
        if not self.klipper_printers:
            return []
        try:
            return json.loads(self.klipper_printers)
        except (json.JSONDecodeError, TypeError):
            return []

    def get_planka_notification_targets(self) -> list[tuple[str, int | None]]:
        """Return [(chat_id, thread_id or None), ...] from BOTKA_PLANKA_NOTIFICATION_CHAT_IDS."""
        targets: list[tuple[str, int | None]] = []
        raw = self.planka_notification_chat_ids
        if raw:
            for part in raw.split(","):
                part = part.strip()
                if ":" in part:
                    cid, tid = part.split(":", 1)
                    try:
                        targets.append((cid.strip(), int(tid.strip())))
                    except ValueError:
                        targets.append((part, None))
                else:
                    targets.append((part, None))
        return targets

    model_config = SettingsConfigDict(
        env_file=".env",
        env_prefix="BOTKA_",
        case_sensitive=False,
    )

    @field_validator("bootstrap_resident_ids", mode="before")
    @classmethod
    def parse_bootstrap_ids(cls, value: str | int | Iterable[int] | None) -> list[int]:
        if value is None:
            return []
        if isinstance(value, int):
            return [value]
        if isinstance(value, str):
            items = [part.strip() for part in value.split(",") if part.strip()]
            return [int(part) for part in items]
        return list(value)

    @field_validator("pins_tracked_chat_ids", mode="before")
    @classmethod
    def parse_pins_tracked_ids(
        cls, value: str | int | Iterable[int] | None
    ) -> list[int]:
        if value is None:
            return []
        if isinstance(value, int):
            return [value]
        if isinstance(value, str):
            items = [part.strip() for part in value.split(",") if part.strip()]
            return [int(part) for part in items]
        return list(value)

    @field_validator("allowed_anon_group_ids", mode="before")
    @classmethod
    def parse_allowed_anon_group_ids(
        cls, value: str | int | Iterable[int] | None
    ) -> list[int]:
        if value is None:
            return []
        if isinstance(value, int):
            return [value]
        if isinstance(value, str):
            items = [part.strip() for part in value.split(",") if part.strip()]
            return [int(part) for part in items]
        return list(value)

    @field_validator("good_morning_photo_urls", mode="before")
    @classmethod
    def parse_good_morning_photo_urls(
        cls, value: str | Iterable[str] | None
    ) -> list[str]:
        if value is None:
            return []
        if isinstance(value, str):
            items = [part.strip() for part in value.split(",") if part.strip()]
            return items
        return list(value)

    @field_validator("mac_tracker_allowed_subnets", mode="before")
    @classmethod
    def parse_mac_tracker_allowed_subnets(
        cls, value: str | Iterable[str] | None
    ) -> list[str]:
        if value is None:
            return []
        if isinstance(value, str):
            items = [part.strip() for part in value.split(",") if part.strip()]
            return items
        return list(value)

    @field_validator(
        "shopping_topic_id",
        "shopping_chat_id",
        "borrowed_topic_id",
        "borrowed_chat_id",
        "pins_chat_id",
        "mac_tracker_notify_chat_id",
        "mac_tracker_notify_topic_id",
        "ups_report_chat_id",
        "ups_report_topic_id",
        "decisions_chat_id",
        "decisions_topic_id",
        "meeting_chat_id",
        "meeting_topic_id",
        "heartbeat_chat_id",
        "heartbeat_topic_id",
        "good_morning_chat_id",
        "good_morning_topic_id",
        mode="before",
    )
    @classmethod
    def parse_optional_int(cls, value: str | int | None) -> int | None:
        if value is None:
            return None
        if isinstance(value, str):
            if not value.strip():
                return None
            return int(value)
        return value
