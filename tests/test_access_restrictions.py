from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from botka.db.models import UserTier
from botka.handlers.bambu.commands import _do_printers
from botka.handlers.mac_tracker.commands import status_handler
from botka.handlers.shopping.commands import need_handler
from botka.handlers.shopping.messages import topic_list_handler


@pytest.mark.asyncio
async def test_need_rejects_guest_user(settings) -> None:
    message = SimpleNamespace(
        from_user=SimpleNamespace(id=101, username="guest_user"),
        reply=AsyncMock(),
    )
    command = SimpleNamespace(args="milk")
    shopping_service = SimpleNamespace()

    await need_handler.__dishka_orig_func__(
        message,
        command,
        settings,
        shopping_service,
        SimpleNamespace(),
        user_record=SimpleNamespace(tier=UserTier.guest),
    )

    message.reply.assert_awaited_once_with(
        "Only residents and members can manage the shopping list."
    )


@pytest.mark.asyncio
async def test_topic_list_rejects_guest_user(settings) -> None:
    message = SimpleNamespace(
        text="- milk\n- bread",
        message_thread_id=settings.shopping_topic_id,
        from_user=SimpleNamespace(id=101, username="guest_user"),
        reply=AsyncMock(),
    )
    shopping_service = SimpleNamespace(
        extract_dash_items=AsyncMock(),
        add_items=AsyncMock(),
        list_open_items=AsyncMock(),
    )

    await topic_list_handler.__dishka_orig_func__(
        message,
        settings,
        shopping_service,
        SimpleNamespace(),
        user_record=SimpleNamespace(tier=UserTier.guest),
    )

    message.reply.assert_awaited_once_with(
        "Only residents and members can manage the shopping list."
    )
    shopping_service.add_items.assert_not_awaited()


@pytest.mark.asyncio
async def test_status_rejects_guest_user() -> None:
    message = SimpleNamespace(
        reply=AsyncMock(),
    )
    user_service = SimpleNamespace(list_users_by_ids=AsyncMock())
    mac_tracker = SimpleNamespace(
        list_present_users=AsyncMock(),
        get_active_lease_seen_map=AsyncMock(),
    )

    await status_handler.__dishka_orig_func__(
        message,
        user_service,
        mac_tracker,
        user_record=SimpleNamespace(tier=UserTier.guest),
    )

    message.reply.assert_awaited_once_with(
        "Only residents and members can view who is in the space."
    )
    mac_tracker.list_present_users.assert_not_awaited()
    user_service.list_users_by_ids.assert_not_awaited()


@pytest.mark.asyncio
async def test_printers_reject_guest_user() -> None:
    message = SimpleNamespace(reply=AsyncMock())
    printer_service = SimpleNamespace(is_configured=True, get_all_statuses=AsyncMock())

    await _do_printers(
        message,
        printer_service,
        user_record=SimpleNamespace(tier=UserTier.guest),
    )

    message.reply.assert_awaited_once_with(
        "Only residents and members can check printer status."
    )
    printer_service.get_all_statuses.assert_not_awaited()


@pytest.mark.asyncio
async def test_printers_reject_none_user() -> None:
    message = SimpleNamespace(reply=AsyncMock())
    printer_service = SimpleNamespace(is_configured=True, get_all_statuses=AsyncMock())

    await _do_printers(message, printer_service, user_record=None)

    message.reply.assert_awaited_once_with(
        "Only residents and members can check printer status."
    )
    printer_service.get_all_statuses.assert_not_awaited()
