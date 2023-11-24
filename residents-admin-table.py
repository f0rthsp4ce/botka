#!/usr/bin/env python3

from __future__ import annotations

import asyncio
import sqlite3
import sys
import typing

import telethon.hints
import telethon.tl.functions.channels
import telethon.tl.types
import yaml
from telethon import TelegramClient

API_ID = 27161938
API_HASH = "25540bdf9a27dc0da066770a1d5b12c5"
DB_FILENAME = "db.sqlite3"
SESSION_NAME = "session"


class WatchingChat(typing.TypedDict):
    id: int
    internal: bool


async def main() -> None:
    config = yaml.safe_load(open(sys.argv[1]))
    db = sqlite3.connect(f"file:{DB_FILENAME}?mode=ro", uri=True)
    client = await TelegramClient(SESSION_NAME, API_ID, API_HASH).start(
        bot_token=config["telegram"]["token"]
    )
    watching_chats = config["telegram"]["chats"]["resident_owned"]

    async with client:
        res = await fetch_residents_chats_table(db, watching_chats, client)
        print_results(res)


class ResidentsChatsTable(typing.NamedTuple):
    chats: list[tuple[telethon.tl.types.Chat | telethon.tl.types.Channel, bool]]
    rows: list[ResidentsChatsTableRow]


class ResidentsChatsTableRow(typing.NamedTuple):
    user: telethon.tl.types.User | int
    is_resident: bool
    chats: list[typing.Optional[telethon.tl.types.ChatParticipant]]


async def fetch_residents_chats_table(
    db: sqlite3.Connection,
    watching_chats: list[WatchingChat],
    client: TelegramClient,
) -> ResidentsChatsTable:
    result = ResidentsChatsTable([], [])
    resident_ids = db_load_residents(db)

    residents = dict[tuple[int, int], telethon.tl.types.ChatParticipant]()
    entities = dict[int, telethon.tl.types.Chat | telethon.tl.types.Channel]()
    users = dict[int, telethon.tl.types.User]()

    await asyncio.gather(
        *map(
            lambda ch: fetch_chat(client, residents, entities, users, ch),
            watching_chats,
        )
    )

    for resident in resident_ids:
        result.rows.append(
            ResidentsChatsTableRow(
                users.get(resident, resident),
                True,
                [residents.get((ch["id"], resident)) for ch in watching_chats],
            )
        )

    for user in users.values():
        if user.id in resident_ids:
            continue
        result.rows.append(
            ResidentsChatsTableRow(
                user,
                False,
                [residents.get((ch["id"], user.id)) for ch in watching_chats],
            )
        )

    result.chats.extend((entities[ch["id"]], ch["internal"]) for ch in watching_chats)

    return result


async def fetch_chat(
    client: TelegramClient,
    residents: dict[tuple[int, int], telethon.tl.types.ChatParticipant],
    entities: dict[int, telethon.tl.types.Chat | telethon.tl.types.Channel],
    users: dict[int, telethon.tl.types.User],
    ch: WatchingChat,
) -> None:
    chat = await client.get_entity(ch["id"])
    if isinstance(chat, telethon.tl.types.User):
        raise ValueError("User is not supported")
    entities[ch["id"]] = chat
    async for participant in client.iter_participants(
        chat,
        filter=None
        if ch["internal"]
        else telethon.tl.types.ChannelParticipantsAdmins(),
    ):
        residents[(ch["id"], participant.id)] = participant.participant
        users[participant.id] = participant


def print_results(result: ResidentsChatsTable) -> None:
    for n in range(len(result.chats)):
        print(end=f"{n}\ufe0f\u20e3")
    print(" <b>Residents</b>")

    title_non_residents = False

    for resident in result.rows:
        if not resident.is_resident and not title_non_residents:
            print()
            for n in range(len(result.chats)):
                print(end=f"〰️")
            print(" <b>Non-residents</b>")
            title_non_residents = True
        for participant in resident.chats:
            if participant is None:
                print(end="➖")
            elif isinstance(participant, telethon.tl.types.ChannelParticipantCreator):
                print(end="👑")
            elif isinstance(participant, telethon.tl.types.ChannelParticipantAdmin):
                print(end="⭐")
            elif isinstance(participant, telethon.tl.types.ChannelParticipant):
                print(end="👤")
            else:
                print(end="❓")
        print(end=" ")
        if isinstance(resident.user, int):
            print(end=f"id={resident.user}")
        else:
            if resident.user.username:
                print(end=f'<a href="https://t.me/{resident.user.username}">')
            print(end=escape_html(resident.user.first_name or ""))
            if resident.user.last_name:
                print(end=" " + escape_html(resident.user.last_name))
            if resident.user.username:
                print(end=f"</a>")
            if resident.user.bot:
                print(end=" ⚙️")
        print()

    print()
    print("👑 — owner, ⭐ — admin, 👤 — participant/subscriber")
    print("➖ — not present (or not admin for public chats)")

    for n, (ch, is_internal) in enumerate(result.chats):
        print(end=f'{n}\ufe0f\u20e3 — <a href="https://t.me/')
        if isinstance(ch, telethon.tl.types.Channel) and ch.username:
            print(end=ch.username)
        else:
            print(end=f"c/{ch.id}")
        print(end=f'">{escape_html(ch.title)}</a>')
        if not is_internal:
            print(end=" (public)")
        print()


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
