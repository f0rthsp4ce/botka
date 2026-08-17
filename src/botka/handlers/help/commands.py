from __future__ import annotations

from dataclasses import dataclass

from aiogram import Router
from aiogram.filters import Command
from aiogram.types import BotCommand, Message

router = Router(name=__name__)


@dataclass(frozen=True, slots=True)
class CommandInfo:
    command: str
    description: str
    section: str = ""


# Single source of truth for all bot commands.
# Used to generate help text and to register commands via the Telegram API.
COMMANDS: tuple[CommandInfo, ...] = (
    CommandInfo("start", "initialize your user profile"),
    CommandInfo("help", "show this help message"),
    CommandInfo("open", "open main door and gate (residents/members)", "Doors"),
    CommandInfo("open_gate", "open the gate", "Doors"),
    CommandInfo(
        "mac", "get a device registration link (residents/members)", "MAC tracker"
    ),
    CommandInfo(
        "status",
        "list who is currently in the space (residents/members)",
        "MAC tracker",
    ),
    CommandInfo(
        "mac_clear",
        "clear MAC assignments (residents can clear others)",
        "MAC tracker",
    ),
    CommandInfo(
        "visit",
        "manage visits to F0 (residents/members); type /visit for more info",
        "Visits",
    ),
    CommandInfo("borrowed", "list borrowed items", "Borrowed items"),
    CommandInfo(
        "need", "add item to the shopping list (residents/members)", "Shopping"
    ),
    CommandInfo(
        "needs",
        "open the pinned shopping list (private chats get a fresh list)",
        "Shopping",
    ),
    CommandInfo(
        "poll",
        "create a public poll; custom options use dash lines (residents only)",
        "Polls",
    ),
    CommandInfo(
        "poll_close",
        "close a poll you created (or reply to the poll/awaiting message)",
        "Polls",
    ),
    CommandInfo(
        "quest",
        "open the quest list, or privately view one by id",
        "Quests (residents/members)",
    ),
    CommandInfo(
        "todo",
        "open the pinned todo list (private chats get a fresh list)",
        "Quests (residents/members)",
    ),
    CommandInfo(
        "task",
        "show full quest details by id, or create a quest from text",
        "Quests (residents/members)",
    ),
    CommandInfo(
        "attach",
        "attach a file to a quest by id or by replying to its bot message",
        "Quests (residents/members)",
    ),
    CommandInfo(
        "take", "accept a quest (move to IN PROGRESS)", "Quests (residents/members)"
    ),
    CommandInfo(
        "abandon",
        "give up a quest (move back to available)",
        "Quests (residents/members)",
    ),
    CommandInfo(
        "doing", "alias for /take (backward compat)", "Quests (residents/members)"
    ),
    CommandInfo(
        "done",
        "complete a quest, or show recent completed quests with no args",
        "Quests (residents/members)",
    ),
    CommandInfo("boards", "list all quest boards", "Quests (residents/members)"),
    CommandInfo("periodic", "list periodic jobs", "Periodic"),
    CommandInfo("periodic_run", "run a periodic job now (residents only)", "Periodic"),
    CommandInfo("ups", "show current UPS / battery status (residents/members)", "UPS"),
    CommandInfo(
        "agenda",
        "add a topic to the weekly meeting agenda (residents/members)",
        "Meeting",
    ),
    CommandInfo(
        "fridge",
        "open the fridge and charge your account (residents/members)",
        "Fridge",
    ),
    CommandInfo("user", "view or set user tier (or reply)", "Administration"),
    CommandInfo(
        "transfer",
        "send money (creates a draft, you confirm)",
        "Finance (refinance)",
    ),
    CommandInfo(
        "request",
        "request money from someone (they confirm or deny)",
        "Finance (refinance)",
    ),
    CommandInfo(
        "balance",
        "show balance and recent activity",
        "Finance (refinance)",
    ),
    CommandInfo(
        "deposit",
        "top up your balance with a card",
        "Finance (refinance)",
    ),
    CommandInfo(
        "transactions", "show your last 10 transactions", "Finance (refinance)"
    ),
    CommandInfo(
        "3d",
        "show 3D printer statuses and cameras (residents/members)",
        "Printers",
    ),
)


def build_help_text() -> str:
    lines: list[str] = ["Available commands:\n"]
    current_section = ""
    for cmd in COMMANDS:
        if cmd.section != current_section:
            current_section = cmd.section
            if current_section:
                lines.append(f"\n{current_section}:")
            else:
                lines.append("")
        lines.append(f"/{cmd.command} — {cmd.description}.")
    return "\n".join(lines)


def get_bot_commands() -> list[BotCommand]:
    return [
        BotCommand(command=cmd.command, description=cmd.description) for cmd in COMMANDS
    ]


HELP_TEXT = build_help_text()


@router.message(Command("help"))
async def help_handler(message: Message) -> None:
    await message.answer(HELP_TEXT)
