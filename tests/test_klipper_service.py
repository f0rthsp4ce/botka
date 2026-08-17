from __future__ import annotations

import httpx
import pytest

from botka.services.klipper_service import KlipperPrinterConfig, KlipperService


def _config(**overrides) -> KlipperPrinterConfig:
    values = {
        "name": "Voron",
        "base_url": "http://voron.local",
        "api_key": "secret",
        "camera_url": None,
    }
    values.update(overrides)
    return KlipperPrinterConfig(**values)


@pytest.mark.asyncio
async def test_status_reads_moonraker_print_state() -> None:
    async def handler(request: httpx.Request) -> httpx.Response:
        assert request.headers["X-Api-Key"] == "secret"
        assert request.url.path == "/printer/objects/query"
        return httpx.Response(
            200,
            json={
                "result": {
                    "status": {
                        "print_stats": {
                            "state": "printing",
                            "filename": "part.gcode",
                            "print_duration": 1800,
                        },
                        "webhooks": {"state": "ready"},
                        "display_status": {"progress": 0.5},
                        "virtual_sdcard": {"progress": 0.5},
                    }
                }
            },
        )

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    service = KlipperService([_config()], timeout=1, client=client)

    status = await service.get_status("Voron")

    assert status is not None
    assert status.connected is True
    assert status.state == "printing"
    assert status.percentage == 50
    assert status.remaining_minutes == 30
    assert status.file_name == "part.gcode"
    await client.aclose()


@pytest.mark.asyncio
async def test_status_returns_offline_status_when_moonraker_is_unavailable() -> None:
    async def handler(request: httpx.Request) -> httpx.Response:
        raise httpx.ConnectError("offline", request=request)

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    service = KlipperService([_config()], timeout=1, client=client)

    status = await service.get_status("Voron")

    assert status is not None
    assert status.connected is False
    assert "offline" in status.format_text()
    await client.aclose()


@pytest.mark.asyncio
async def test_status_reports_klippy_error_as_offline() -> None:
    async def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200,
            json={
                "result": {
                    "status": {
                        "webhooks": {
                            "state": "error",
                            "state_message": "MCU is not connected",
                        }
                    }
                }
            },
        )

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    service = KlipperService([_config()], timeout=1, client=client)

    status = await service.get_status("Voron")

    assert status is not None
    assert status.connected is False
    assert status.state == "error"
    assert status.error_message == "MCU is not connected"
    await client.aclose()


@pytest.mark.asyncio
async def test_photo_uses_discovered_moonraker_webcam() -> None:
    requests: list[httpx.Request] = []

    async def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        if request.url.path == "/server/webcams/list":
            return httpx.Response(
                200,
                json={
                    "result": {
                        "webcams": [
                            {
                                "enabled": True,
                                "snapshot_url": "/webcam/?action=snapshot",
                            }
                        ]
                    }
                },
            )
        assert request.url.path == "/webcam/"
        assert request.url.params["action"] == "snapshot"
        return httpx.Response(200, content=b"jpeg")

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    service = KlipperService([_config()], timeout=1, client=client)

    photo = await service.get_photo("Voron")

    assert photo == b"jpeg"
    assert all(request.headers["X-Api-Key"] == "secret" for request in requests)
    await client.aclose()


@pytest.mark.asyncio
async def test_photo_uses_configured_camera_url_without_discovery() -> None:
    async def handler(request: httpx.Request) -> httpx.Response:
        assert request.url == httpx.URL("http://camera.local/snapshot.jpg")
        assert "X-Api-Key" not in request.headers
        return httpx.Response(200, content=b"jpeg")

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    service = KlipperService(
        [_config(camera_url="http://camera.local/snapshot.jpg")],
        timeout=1,
        client=client,
    )

    assert await service.get_photo("Voron") == b"jpeg"
    await client.aclose()
