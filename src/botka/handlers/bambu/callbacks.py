from __future__ import annotations

import asyncio
import logging
from datetime import datetime

from aiogram import F, Router
from aiogram.exceptions import TelegramBadRequest
from aiogram.types import BufferedInputFile, CallbackQuery, InputMediaPhoto
from dishka.integrations.aiogram import FromDishka, inject

from botka.db.models import User, UserTier
from botka.handlers.bambu.utils import camera_keyboard, status_keyboard
from botka.services.printer_service import PrinterService

logger = logging.getLogger(__name__)
router = Router(name=__name__)


def _updated_at() -> str:
    return f"Updated {datetime.now().strftime('%H:%M:%S')}"


@router.callback_query(F.data.in_({"printer_refresh", "bambu_refresh"}))
@inject
async def printer_refresh_callback(
    callback: CallbackQuery,
    printer_service: FromDishka[PrinterService],
    user_record: User | None = None,
) -> None:
    tier = user_record.tier if user_record else UserTier.guest
    if tier not in (UserTier.resident, UserTier.member):
        await callback.answer("Access denied.", show_alert=True)
        return
    statuses = await printer_service.get_all_statuses()
    if not statuses:
        await callback.answer()
        return
    text = "\n\n".join(s.format_text() for s in statuses)
    kb = status_keyboard(printer_service.printer_names)
    answer_text = _updated_at()
    if callback.message is not None:
        try:
            await callback.message.edit_text(text, reply_markup=kb)
        except TelegramBadRequest as e:
            if "message is not modified" not in str(e):
                raise
            answer_text = "Already up to date"
    await callback.answer(answer_text)


@router.callback_query(F.data.startswith("bambu_cam:"))
@router.callback_query(F.data.startswith("printer_cam:"))
@inject
async def printer_camera_callback(
    callback: CallbackQuery,
    printer_service: FromDishka[PrinterService],
    user_record: User | None = None,
) -> None:
    if callback.data is None:
        await callback.answer("Invalid request.", show_alert=True)
        return
    tier = user_record.tier if user_record else UserTier.guest
    if tier not in (UserTier.resident, UserTier.member):
        await callback.answer(
            "Only residents and members can view printer camera.",
            show_alert=True,
        )
        return
    name = callback.data.split(":", 1)[1]
    await callback.answer(f"Fetching photo from {name}…")
    photo, status = await asyncio.gather(
        printer_service.get_photo(name),
        printer_service.get_status(name),
    )
    if photo is None:
        if callback.message is not None:
            await callback.message.reply(
                f"⚠️ Could not get camera photo from <b>{name}</b> "
                "(timeout or camera unavailable)."
            )
        return
    caption = status.format_text() if status is not None else f"📷 {name}"
    if callback.message is not None:
        await callback.message.reply_photo(
            BufferedInputFile(photo, filename="printer_cam.jpg"),
            caption=caption,
            reply_markup=camera_keyboard(name),
        )


@router.callback_query(F.data.startswith("bambu_cam_refresh:"))
@router.callback_query(F.data.startswith("printer_cam_refresh:"))
@inject
async def printer_camera_refresh_callback(
    callback: CallbackQuery,
    printer_service: FromDishka[PrinterService],
    user_record: User | None = None,
) -> None:
    if callback.data is None:
        await callback.answer("Invalid request.", show_alert=True)
        return
    tier = user_record.tier if user_record else UserTier.guest
    if tier not in (UserTier.resident, UserTier.member):
        await callback.answer("Access denied.", show_alert=True)
        return
    name = callback.data.split(":", 1)[1]
    photo, status = await asyncio.gather(
        printer_service.get_photo(name),
        printer_service.get_status(name),
    )
    if photo is None:
        await callback.answer("Camera unavailable.", show_alert=True)
        return
    caption = status.format_text() if status is not None else f"📷 {name}"
    answer_text = _updated_at()
    if callback.message is not None:
        try:
            await callback.message.edit_media(
                InputMediaPhoto(
                    media=BufferedInputFile(photo, filename="printer_cam.jpg"),
                    caption=caption,
                ),
                reply_markup=camera_keyboard(name),
            )
        except TelegramBadRequest as e:
            if "message is not modified" not in str(e):
                raise
            answer_text = "Already up to date"
    await callback.answer(answer_text)
