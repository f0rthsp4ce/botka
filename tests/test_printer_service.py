from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from botka.services.printer_service import PrinterService


def _backend(names: list[str]):
    return SimpleNamespace(
        is_configured=bool(names),
        printer_names=names,
        connect_all=AsyncMock(),
        disconnect_all=AsyncMock(),
        close=AsyncMock(),
        get_status=AsyncMock(side_effect=lambda name: f"status:{name}"),
        get_photo=AsyncMock(side_effect=lambda name: f"photo:{name}".encode()),
    )


@pytest.mark.asyncio
async def test_routes_each_printer_to_its_backend() -> None:
    bambu = _backend(["A1"])
    klipper = _backend(["Voron"])
    service = PrinterService(bambu, klipper)

    assert service.printer_names == ["A1", "Voron"]
    assert await service.get_all_statuses() == ["status:A1", "status:Voron"]
    assert await service.get_photo("Voron") == b"photo:Voron"
    bambu.get_status.assert_awaited_once_with("A1")
    klipper.get_status.assert_awaited_once_with("Voron")
    klipper.get_photo.assert_awaited_once_with("Voron")


def test_rejects_duplicate_names_across_backends() -> None:
    with pytest.raises(ValueError, match="Duplicate printer name: Shared"):
        PrinterService(_backend(["Shared"]), _backend(["Shared"]))
