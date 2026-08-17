from __future__ import annotations

import asyncio
from typing import Protocol

from botka.config import Settings
from botka.services.bambu_service import BambuPrinterStatus, BambuService
from botka.services.klipper_service import KlipperPrinterStatus, KlipperService


class PrinterBackend(Protocol):
    @property
    def printer_names(self) -> list[str]: ...

    async def get_status(
        self, name: str
    ) -> BambuPrinterStatus | KlipperPrinterStatus | None: ...

    async def get_photo(self, name: str) -> bytes | None: ...


PrinterStatus = BambuPrinterStatus | KlipperPrinterStatus


class PrinterService:
    """Combines all configured printer backends behind one bot interface."""

    def __init__(self, bambu: BambuService, klipper: KlipperService) -> None:
        self._bambu = bambu
        self._klipper = klipper
        self._backends: dict[str, PrinterBackend] = {}
        for backend in (bambu, klipper):
            for name in backend.printer_names:
                if name in self._backends:
                    raise ValueError(f"Duplicate printer name: {name}")
                self._backends[name] = backend

    @classmethod
    def from_settings(cls, settings: Settings) -> PrinterService:
        return cls(
            BambuService.from_settings(settings),
            KlipperService.from_settings(settings),
        )

    @property
    def is_configured(self) -> bool:
        return bool(self._backends)

    @property
    def printer_names(self) -> list[str]:
        return list(self._backends)

    async def start(self) -> None:
        if self._bambu.is_configured:
            await self._bambu.connect_all()

    async def close(self) -> None:
        if self._bambu.is_configured:
            await self._bambu.disconnect_all()
        await self._klipper.close()

    async def get_status(self, name: str) -> PrinterStatus | None:
        backend = self._backends.get(name)
        return await backend.get_status(name) if backend is not None else None

    async def get_all_statuses(self) -> list[PrinterStatus]:
        statuses = await asyncio.gather(
            *(self.get_status(name) for name in self._backends)
        )
        return [status for status in statuses if status is not None]

    async def get_photo(self, name: str) -> bytes | None:
        backend = self._backends.get(name)
        return await backend.get_photo(name) if backend is not None else None
