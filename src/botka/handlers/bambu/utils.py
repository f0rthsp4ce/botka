from __future__ import annotations

from aiogram.types import InlineKeyboardButton, InlineKeyboardMarkup


def status_keyboard(printer_names: list[str]) -> InlineKeyboardMarkup:
    """Inline keyboard for the all-printers status message."""
    rows = [
        [InlineKeyboardButton(text=f"📷 {name}", callback_data=f"printer_cam:{name}")]
        for name in printer_names
    ]
    rows.append(
        [InlineKeyboardButton(text="🔄 Refresh", callback_data="printer_refresh")]
    )
    return InlineKeyboardMarkup(inline_keyboard=rows)


def camera_keyboard(name: str) -> InlineKeyboardMarkup:
    """Inline keyboard attached to a camera photo message."""
    return InlineKeyboardMarkup(
        inline_keyboard=[
            [
                InlineKeyboardButton(
                    text="🔄 Refresh",
                    callback_data=f"printer_cam_refresh:{name}",
                )
            ]
        ]
    )
