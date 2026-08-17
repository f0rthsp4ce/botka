from __future__ import annotations

import asyncio
import html
import logging
from dataclasses import dataclass
from urllib.parse import urljoin, urlsplit

import httpx

from botka.config import Settings

logger = logging.getLogger(__name__)


@dataclass(frozen=True, slots=True)
class KlipperPrinterConfig:
    name: str
    base_url: str
    api_key: str | None = None
    camera_url: str | None = None


@dataclass(frozen=True, slots=True)
class KlipperPrinterStatus:
    name: str
    connected: bool
    state: str
    percentage: int | None
    remaining_minutes: int | None
    file_name: str | None
    error_message: str | None = None

    def _state_icon(self) -> str:
        if not self.connected:
            return "📵"
        return {
            "printing": "🖨️",
            "paused": "⏸️",
            "complete": "✅",
            "error": "❌",
            "cancelled": "❌",
            "standby": "💤",
        }.get(self.state.lower(), "❓")

    def format_text(self) -> str:
        state = html.escape(self.state)
        if not self.connected:
            state += " (offline)"
        parts = [f"{self._state_icon()} <b>{html.escape(self.name)}</b>", state]
        if self.percentage is not None:
            parts.append(f"{self.percentage}%")
        if self.remaining_minutes is not None and self.remaining_minutes > 0:
            hours, minutes = divmod(self.remaining_minutes, 60)
            parts.append(f"{hours}h {minutes}m left" if hours else f"{minutes}m left")

        lines = [" · ".join(parts)]
        if self.file_name:
            lines.append(f"📄 <code>{html.escape(self.file_name)}</code>")
        if self.error_message:
            lines.append(f"⚠️ {html.escape(self.error_message)}")
        return "\n".join(lines)


class KlipperService:
    """Reads Klipper status and webcam snapshots through Moonraker."""

    def __init__(
        self,
        configs: list[KlipperPrinterConfig],
        timeout: float,
        *,
        client: httpx.AsyncClient | None = None,
    ) -> None:
        self._configs = {config.name: config for config in configs}
        self._owns_client = client is None
        self._client = client or httpx.AsyncClient(
            timeout=httpx.Timeout(timeout),
            follow_redirects=True,
        )

    @classmethod
    def from_settings(cls, settings: Settings) -> KlipperService:
        configs = [
            KlipperPrinterConfig(
                name=item["name"],
                base_url=item["base_url"],
                api_key=item.get("api_key"),
                camera_url=item.get("camera_url"),
            )
            for item in settings.get_klipper_printer_configs()
        ]
        return cls(configs, settings.klipper_timeout_seconds)

    @property
    def is_configured(self) -> bool:
        return bool(self._configs)

    @property
    def printer_names(self) -> list[str]:
        return list(self._configs)

    @staticmethod
    def _headers(config: KlipperPrinterConfig) -> dict[str, str]:
        return {"X-Api-Key": config.api_key} if config.api_key else {}

    @staticmethod
    def _url(config: KlipperPrinterConfig, path: str) -> str:
        return urljoin(f"{config.base_url.rstrip('/')}/", path.lstrip("/"))

    async def get_status(self, name: str) -> KlipperPrinterStatus | None:
        config = self._configs.get(name)
        if config is None:
            return None
        try:
            response = await self._client.get(
                self._url(
                    config,
                    "/printer/objects/query"
                    "?webhooks&print_stats&display_status&virtual_sdcard",
                ),
                headers=self._headers(config),
            )
            response.raise_for_status()
            status = response.json()["result"]["status"]
            webhooks = status.get("webhooks", {})
            klippy_state = webhooks.get("state")
            if klippy_state and klippy_state != "ready":
                message = webhooks.get("state_message") or webhooks.get("message")
                return KlipperPrinterStatus(
                    name=name,
                    connected=False,
                    state=str(klippy_state),
                    percentage=None,
                    remaining_minutes=None,
                    file_name=None,
                    error_message=str(message) if message else None,
                )
            print_stats = status.get("print_stats", {})
            progress = status.get("display_status", {}).get("progress")
            if not isinstance(progress, (int, float)):
                progress = status.get("virtual_sdcard", {}).get("progress")
            percentage = (
                max(0, min(100, round(progress * 100)))
                if isinstance(progress, (int, float))
                else None
            )
            duration = print_stats.get("print_duration")
            remaining_minutes: int | None = None
            if (
                isinstance(duration, (int, float))
                and isinstance(progress, (int, float))
                and 0 < progress < 1
            ):
                remaining_minutes = max(
                    1,
                    round(duration * (1 - progress) / progress / 60),
                )
            message = print_stats.get("message") or None
            return KlipperPrinterStatus(
                name=name,
                connected=True,
                state=str(print_stats.get("state") or "unknown"),
                percentage=percentage,
                remaining_minutes=remaining_minutes,
                file_name=print_stats.get("filename") or None,
                error_message=str(message) if message else None,
            )
        except (httpx.HTTPError, AttributeError, KeyError, TypeError, ValueError):
            logger.exception("Failed to get status from Klipper printer: %s", name)
            return KlipperPrinterStatus(
                name=name,
                connected=False,
                state="unknown",
                percentage=None,
                remaining_minutes=None,
                file_name=None,
            )

    async def get_all_statuses(self) -> list[KlipperPrinterStatus]:
        statuses = await asyncio.gather(
            *(self.get_status(name) for name in self._configs)
        )
        return [status for status in statuses if status is not None]

    async def _discover_camera_url(
        self, config: KlipperPrinterConfig
    ) -> str | None:
        if config.camera_url:
            return urljoin(f"{config.base_url.rstrip('/')}/", config.camera_url)
        response = await self._client.get(
            self._url(config, "/server/webcams/list"),
            headers=self._headers(config),
        )
        response.raise_for_status()
        webcams = response.json().get("result", {}).get("webcams", [])
        webcam = next(
            (
                item
                for item in webcams
                if item.get("enabled", True) and item.get("snapshot_url")
            ),
            None,
        )
        if webcam is None:
            return None
        return urljoin(
            f"{config.base_url.rstrip('/')}/", str(webcam["snapshot_url"])
        )

    def _camera_headers(
        self, config: KlipperPrinterConfig, camera_url: str
    ) -> dict[str, str]:
        camera_origin = urlsplit(camera_url)
        moonraker_origin = urlsplit(config.base_url)
        if (camera_origin.scheme, camera_origin.netloc) == (
            moonraker_origin.scheme,
            moonraker_origin.netloc,
        ):
            return self._headers(config)
        return {}

    async def get_photo(self, name: str) -> bytes | None:
        config = self._configs.get(name)
        if config is None:
            return None
        try:
            camera_url = await self._discover_camera_url(config)
            if camera_url is None:
                return None
            response = await self._client.get(
                camera_url,
                headers=self._camera_headers(config, camera_url),
            )
            response.raise_for_status()
            return response.content or None
        except (httpx.HTTPError, AttributeError, KeyError, TypeError, ValueError):
            logger.exception(
                "Failed to get camera photo from Klipper printer: %s", name
            )
            return None

    async def close(self) -> None:
        if self._owns_client:
            await self._client.aclose()
