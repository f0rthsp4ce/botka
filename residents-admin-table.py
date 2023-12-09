#!/usr/bin/env python3

from __future__ import annotations

import asyncio
import sqlite3
import sys
import traceback
import typing

import telethon
import telethon.hints
import telethon.tl.functions.channels
import telethon.tl.types
import yaml

API_ID = 27161938
API_HASH = "25540bdf9a27dc0da066770a1d5b12c5"
DB_FILENAME = "db.sqlite3"
SESSION_NAME = "session"


TypeParticipant: typing.TypeAlias = (
    telethon.tl.types.TypeChannelParticipant
    | telethon.tl.types.TypeChatParticipant
)


class WatchingChat(typing.TypedDict):
    id: int
    internal: bool


async def main() -> None:
    with open(sys.argv[1]) as f:  # noqa: ASYNC101
        config = yaml.safe_load(f)

    db = sqlite3.connect(f"file:{DB_FILENAME}?mode=ro", uri=True)
    client = await telethon.TelegramClient(
        SESSION_NAME, API_ID, API_HASH
    ).start(bot_token=config["telegram"]["token"])
    watching_chats = config["telegram"]["chats"]["resident_owned"]

    async with client:
        res = await fetch_residents_chats_table(client, watching_chats)
        print_results(res, db_load_residents(db))


class ResidentsChatsTable(typing.NamedTuple):
    chats: list[FetchChatResult]
    users: dict[int, telethon.tl.types.User]
    errors: list[int]


class FetchChatResult(typing.NamedTuple):
    chat: telethon.tl.types.Chat | telethon.tl.types.Channel
    is_internal: bool
    participants: dict[int, tuple[telethon.tl.types.User, TypeParticipant]]


async def fetch_residents_chats_table(
    client: telethon.TelegramClient,
    watching_chats: list[WatchingChat],
) -> ResidentsChatsTable:
    result = ResidentsChatsTable([], {}, [])
    for wc, ch in zip(
        watching_chats,
        await asyncio.gather(
            *(fetch_chat(client, ch) for ch in watching_chats),
            return_exceptions=True,
        ),
    ):
        if isinstance(ch, FetchChatResult):
            result.chats.append(ch)
            for u, _ in ch.participants.values():
                result.users[u.id] = u
        else:
            traceback.print_exception(type(ch), ch, ch.__traceback__)
            result.errors.append(wc["id"])
    return result


async def fetch_chat(
    client: telethon.TelegramClient, ch: WatchingChat
) -> FetchChatResult:
    chat = await client.get_entity(ch["id"])
    if isinstance(chat, telethon.tl.types.User):
        msg = "User is not supported"
        raise TypeError(msg)
    result = FetchChatResult(chat, ch["internal"], {})
    async for user in client.iter_participants(
        chat,
        filter=None
        if ch["internal"]
        else telethon.tl.types.ChannelParticipantsAdmins(),
    ):
        result.participants[user.id] = (user, user.participant)
    return result


def print_results(  # noqa: C901 PLR0912
    result: ResidentsChatsTable, resident_ids: list[int]
) -> None:
    def key(user_id: int) -> tuple[int, str, int]:
        if user_id in resident_ids:
            return (0, "Residents", resident_ids.index(user_id))
        user = result.users.get(user_id)
        if user is not None and user.bot is True:
            return (1, "Bots", user_id)
        return (2, "Non-residents", user_id)

    first = True
    prev_table = None
    for user_id in sorted(result.users.keys() | set(resident_ids), key=key):
        # Print table header
        curr_table = key(user_id)[1]
        if curr_table != prev_table:
            if not first:
                print()
            first = False
            row = [f"{n}\ufe0f\u20e3" for n in range(len(result.chats))]
            print(f"{format_row(row)} <b>{curr_table}</b>")
            prev_table = curr_table

        row = [format_participant(user_id, ch) for ch in result.chats]
        print(end=format_row(row) + " ")

        if (user := result.users.get(user_id)) is None:
            print(end=f"id={user_id}")
        else:
            if user.username:
                print(end=f'<a href="https://t.me/{user.username}">')
            print(end=escape_html(user.first_name or ""))
            if user.last_name:
                print(end=" " + escape_html(user.last_name))
            if user.username:
                print(end="</a>")
        print()

    print()

    print("<b>Legend</b>")

    for n, ch in enumerate(result.chats):
        print(
            end=format_row(
                [
                    "〰️" if ni < n else f"{n}\ufe0f\u20e3" if ni == n else ""
                    for ni in range(len(result.chats))
                ]
            ).rstrip()
        )
        print(end=' — <a href="https://t.me/')
        if isinstance(ch, telethon.tl.types.Channel) and ch.username:
            print(end=ch.username)
        else:
            print(end=f"c/{ch.chat.id}")
        print(end=f'">{escape_html(ch.chat.title)}</a>')
        if not ch.is_internal:
            print(end=" (public)")
        print()

    print("👑 — owner, ⭐ — admin, 👤 — participant/subscriber")
    print("➖ — not present (or not admin for public chats)")

    if result.errors:
        print(f"\n⚠️ got errors while fetching chats with ids {result.errors}")


def format_row(items: list[str]) -> str:
    middle = len(items) // 2
    return "".join(items[0:middle]) + "  " + "".join(items[middle:])


t = telethon.tl.types
PARTICIPANT_TYPES = [
    (None | t.ChannelParticipantBanned | t.ChannelParticipantLeft, "➖"),
    (t.ChannelParticipant | t.ChatParticipant | t.ChatParticipant, "👤"),
    (t.ChannelParticipantCreator | t.ChatParticipantCreator, "👑"),
    (t.ChannelParticipantAdmin | t.ChatParticipantAdmin, "⭐"),
    (t.ChannelParticipantSelf, "❓"),
]


def format_participant(user_id: int, ch: FetchChatResult) -> str:
    p = ch.participants.get(user_id)
    p = p[1] if p else None
    return next((s for t, s in PARTICIPANT_TYPES if isinstance(p, t)), "❓")


def escape_html(s: str) -> str:
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def db_load_residents(db: sqlite3.Connection) -> list[int]:
    return [
        row[0]
        for row in db.execute(
            r"""
                  SELECT tg_id
                    FROM residents
                   WHERE end_date IS NULL
                ORDER BY begin_date DESC
            """
        ).fetchall()
    ]


if __name__ == "__main__":
    asyncio.run(main())
