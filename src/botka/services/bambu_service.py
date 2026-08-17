from __future__ import annotations

import asyncio
import base64
import logging
import time
from dataclasses import dataclass
from threading import Lock

import bambulabs_api as bl
from bambulabs_api.states_info import GcodeState

from botka.config import Settings

logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class BambuPrinterConfig:
    name: str
    ip: str
    serial: str
    access_code: str


@dataclass(frozen=True)
class BambuPrinterStatus:
    name: str
    connected: bool
    gcode_state: GcodeState
    # percentage 0–100, or None if unknown
    percentage: int | None
    # remaining time in minutes (from mc_remaining_time MQTT field), or None
    remaining_minutes: int | None
    file_name: str | None
    error_code: int

    def _state_icon(self) -> str:
        if not self.connected:
            return "📵"
        match self.gcode_state:
            case GcodeState.RUNNING:
                return "🖨️"
            case GcodeState.PAUSE:
                return "⏸️"
            case GcodeState.FINISH:
                return "✅"
            case GcodeState.FAILED:
                return "❌"
            case GcodeState.IDLE:
                return "💤"
            case _:
                return "❓"

    def format_text(self) -> str:
        # First line: icon · name · state · progress · times
        parts: list[str] = [f"{self._state_icon()} <b>{self.name}</b>"]
        state_str = str(self.gcode_state)
        if not self.connected:
            state_str += " (offline)"
        parts.append(state_str)

        if self.percentage is not None:
            parts.append(f"{self.percentage}%")

        if self.remaining_minutes is not None and self.remaining_minutes > 0:
            r_h, r_m = divmod(self.remaining_minutes, 60)
            parts.append(f"{r_h}h {r_m}m left" if r_h else f"{r_m}m left")
            pct = self.percentage
            if pct and 0 < pct < 100:
                elapsed = self.remaining_minutes * pct // (100 - pct)
                e_h, e_m = divmod(elapsed, 60)
                parts.append(f"{e_h}h {e_m}m elapsed" if e_h else f"{e_m}m elapsed")

        lines: list[str] = [" · ".join(parts)]

        if self.file_name:
            lines.append(f"📄 <code>{self.file_name}</code>")
        if self.error_code:
            lines.append(f"⚠️ <code>{self.error_code:#010x}</code>")
        return "\n".join(lines)


class BambuService:
    """Manages persistent MQTT + camera connections to one or more Bambu Lab
    printers operating in LAN mode with access-code (PIN) authentication."""

    def __init__(
        self,
        configs: list[BambuPrinterConfig],
        camera_timeout: float,
    ) -> None:
        self._camera_timeout = camera_timeout
        self._printers: dict[str, bl.Printer] = {
            cfg.name: bl.Printer(cfg.ip, cfg.access_code, cfg.serial) for cfg in configs
        }
        self._camera_start_locks = {name: Lock() for name in self._printers}

    @classmethod
    def from_settings(cls, settings: Settings) -> BambuService:
        configs: list[BambuPrinterConfig] = [
            BambuPrinterConfig(
                name=item["name"],
                ip=item["ip"],
                serial=item["serial"],
                access_code=item["access_code"],
            )
            for item in settings.get_bambu_printer_configs()
        ]
        return cls(configs, settings.bambu_camera_timeout_seconds)

    @property
    def is_configured(self) -> bool:
        return bool(self._printers)

    @property
    def printer_names(self) -> list[str]:
        return list(self._printers.keys())

    # ------------------------------------------------------------------ #
    # Lifecycle                                                            #
    # ------------------------------------------------------------------ #

    def _connect_sync(self) -> None:
        for name, printer in self._printers.items():
            try:
                # Printer.connect() also starts a camera worker.  Keep status
                # requests MQTT-only; an unreachable camera otherwise enters a
                # noisy reconnect loop even when nobody requested an image.
                printer.mqtt_start()
                logger.info("Started Bambu MQTT client: %s", name)
            except Exception:
                logger.exception("Failed to connect to Bambu printer: %s", name)

    def _disconnect_sync(self) -> None:
        for name, printer in self._printers.items():
            try:
                printer.disconnect()
            except Exception:
                logger.exception("Failed to disconnect from Bambu printer: %s", name)

    async def connect_all(self) -> None:
        await asyncio.to_thread(self._connect_sync)

    async def disconnect_all(self) -> None:
        await asyncio.to_thread(self._disconnect_sync)

    # ------------------------------------------------------------------ #
    # Status                                                               #
    # ------------------------------------------------------------------ #

    def _get_status_sync(self, name: str) -> BambuPrinterStatus | None:
        printer = self._printers.get(name)
        if printer is None:
            return None
        # All getters below read MQTT data cached by the library.  Waiting for
        # an unreachable printer only ties up worker threads and makes /3d
        # appear to hang, so return an explicit offline status immediately.
        if not printer.mqtt_client_ready():
            return BambuPrinterStatus(
                name=name,
                connected=False,
                gcode_state=GcodeState.UNKNOWN,
                percentage=None,
                remaining_minutes=None,
                file_name=None,
                error_code=0,
            )
        try:
            gcode_state = printer.get_state()
            raw_pct = printer.get_percentage()
            percentage: int | None = (
                int(raw_pct) if isinstance(raw_pct, (int, float)) else None
            )
            raw_time = printer.get_time()
            remaining_minutes: int | None = (
                int(raw_time) if isinstance(raw_time, (int, float)) else None
            )
            # Prefer the human-readable subtask name; fall back to gcode filename
            file_name = printer.subtask_name() or printer.get_file_name() or None
            error_code = printer.print_error_code()
            return BambuPrinterStatus(
                name=name,
                connected=printer.mqtt_client_connected(),
                gcode_state=gcode_state,
                percentage=percentage,
                remaining_minutes=remaining_minutes,
                file_name=file_name,
                error_code=error_code,
            )
        except Exception:
            logger.exception("Failed to get status from printer: %s", name)
            return None

    async def get_status(self, name: str) -> BambuPrinterStatus | None:
        return await asyncio.to_thread(self._get_status_sync, name)

    async def get_all_statuses(self) -> list[BambuPrinterStatus]:
        statuses = await asyncio.gather(
            *(self.get_status(name) for name in self._printers)
        )
        return [s for s in statuses if s is not None]

    # ------------------------------------------------------------------ #
    # Camera                                                               #
    # ------------------------------------------------------------------ #

    def _get_photo_sync(self, name: str) -> bytes | None:
        """Start the requested camera and poll for a JPEG within the timeout."""
        printer = self._printers.get(name)
        if printer is None:
            return None
        try:
            # PrinterCamera.start() is idempotent but not internally locked.
            with self._camera_start_locks[name]:
                printer.camera_start()
        except Exception:
            logger.exception("Failed to start camera for printer: %s", name)
            return None
        deadline = time.monotonic() + self._camera_timeout
        while time.monotonic() < deadline:
            try:
                frame_b64 = printer.get_camera_frame()
                return base64.b64decode(frame_b64)
            except Exception:
                time.sleep(0.25)
        logger.warning("Camera frame timeout for printer: %s", name)
        return None

    async def get_photo(self, name: str) -> bytes | None:
        return await asyncio.to_thread(self._get_photo_sync, name)
