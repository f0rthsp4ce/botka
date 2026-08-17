from __future__ import annotations

from aiogram import F, Router
from aiogram.filters import Command
from aiogram.types import Message
from dishka.integrations.aiogram import FromDishka, inject

from botka.db.models import User, UserTier
from botka.handlers.bambu.utils import status_keyboard
from botka.handlers.menu import Btn
from botka.services.printer_service import PrinterService

router = Router(name=__name__)


async def _do_printers(
    message: Message,
    printer_service: PrinterService,
    user_record: User | None,
) -> None:
    tier = user_record.tier if user_record else UserTier.guest
    if tier not in (UserTier.resident, UserTier.member):
        await message.reply("Only residents and members can check printer status.")
        return
    if not printer_service.is_configured:
        await message.reply("3D printer integration is not configured.")
        return
    statuses = await printer_service.get_all_statuses()
    if not statuses:
        await message.reply("Could not retrieve printer status.")
        return
    text = "\n\n".join(s.format_text() for s in statuses)
    kb = status_keyboard(printer_service.printer_names)
    await message.reply(text, reply_markup=kb)


@router.message(Command("3d"))
@inject
async def printer_status_handler(
    message: Message,
    printer_service: FromDishka[PrinterService],
    user_record: User | None = None,
) -> None:
    await _do_printers(message, printer_service, user_record)


@router.message(F.text == Btn.BAMBU, F.chat.type == "private")
@inject
async def menu_printers_message(
    message: Message,
    printer_service: FromDishka[PrinterService],
    user_record: User | None = None,
) -> None:
    await _do_printers(message, printer_service, user_record)
