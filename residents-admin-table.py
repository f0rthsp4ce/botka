#!/usr/bin/env python3

from __future__ import annotations

import asyncio
import sqlite3
import sys
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
        res = await fetch_residents_chats_table(db, watching_chats, client)
        print_results(res)


class ResidentsChatsTable(typing.NamedTuple):
    chats: list[tuple[telethon.tl.types.Chat | telethon.tl.types.Channel, bool]]
    rows: list[ResidentsChatsTableRow]


class ResidentsChatsTableRow(typing.NamedTuple):
    user: telethon.tl.types.User | int
    is_resident: bool
    chats: list[TypeParticipant | None]


async def fetch_residents_chats_table(
    db: sqlite3.Connection,
    watching_chats: list[WatchingChat],
    client: telethon.TelegramClient,
) -> ResidentsChatsTable:
    result = ResidentsChatsTable([], [])
    resident_ids = db_load_residents(db)

    residents = dict[tuple[int, int], TypeParticipant]()
    entities = dict[int, telethon.tl.types.Chat | telethon.tl.types.Channel]()
    users = dict[int, telethon.tl.types.User]()

    await asyncio.gather(
        *(
            fetch_chat(client, residents, entities, users, ch)
            for ch in watching_chats
        ),
    )

    for resident in resident_ids:
        result.rows.append(
            ResidentsChatsTableRow(
                user=users.get(resident, resident),
                is_resident=True,
                chats=[
                    residents.get((ch["id"], resident)) for ch in watching_chats
                ],
            )
        )

    for user in users.values():
        if user.id in resident_ids:
            continue
        result.rows.append(
            ResidentsChatsTableRow(
                user=user,
                is_resident=False,
                chats=[
                    residents.get((ch["id"], user.id)) for ch in watching_chats
                ],
            )
        )

    result.chats.extend(
        (entities[ch["id"]], ch["internal"]) for ch in watching_chats
    )

    return result


async def fetch_chat(
    client: telethon.TelegramClient,
    residents: dict[tuple[int, int], TypeParticipant],
    entities: dict[int, telethon.tl.types.Chat | telethon.tl.types.Channel],
    users: dict[int, telethon.tl.types.User],
    ch: WatchingChat,
) -> None:
    chat = await client.get_entity(ch["id"])
    if isinstance(chat, telethon.tl.types.User):
        msg = "User is not supported"
        raise TypeError(msg)
    entities[ch["id"]] = chat
    async for participant in client.iter_participants(
        chat,
        filter=None
        if ch["internal"]
        else telethon.tl.types.ChannelParticipantsAdmins(),
    ):
        residents[(ch["id"], participant.id)] = participant.participant
        users[participant.id] = participant


def print_results(result: ResidentsChatsTable) -> None:  # noqa: C901
    tables: list[
        tuple[str, typing.Callable[[ResidentsChatsTableRow], bool]]
    ] = [
        ("Residents", lambda r: r.is_resident),
        ("Bots", lambda r: not isinstance(r.user, int) and r.user.bot is True),
        ("Non-residents", lambda _: True),
    ]

    table_index: int | None = None
    first = True
    for row in sorted(
        result.rows,
        key=lambda r: next(i for i, (_, f) in enumerate(tables) if f(r)),
    ):
        # Print table header
        while table_index is None or not tables[table_index][1](row):
            table_index = table_index + 1 if table_index is not None else 0
            if not first:
                print()
            first = False
            print(
                end=format_row(
                    [f"{n}\ufe0f\u20e3" for n in range(len(result.chats))]
                )
            )
            print(f" <b>{tables[table_index][0]}</b>")

        print(end=format_row([format_participant(p) for p in row.chats]) + " ")

        if isinstance(row.user, int):
            print(end=f"id={row.user}")
        else:
            if row.user.username:
                print(end=f'<a href="https://t.me/{row.user.username}">')
            print(end=escape_html(row.user.first_name or ""))
            if row.user.last_name:
                print(end=" " + escape_html(row.user.last_name))
            if row.user.username:
                print(end="</a>")
        print()

    print()

    print("<b>Legend</b>")

    for n, (ch, is_internal) in enumerate(result.chats):
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
            print(end=f"c/{ch.id}")
        print(end=f'">{escape_html(ch.title)}</a>')
        if not is_internal:
            print(end=" (public)")
        print()

    print("👑 — owner, ⭐ — admin, 👤 — participant/subscriber")
    print("➖ — not present (or not admin for public chats)")


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


def format_participant(p: TypeParticipant | None) -> str:
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
