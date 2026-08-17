from __future__ import annotations

import logging
from typing import AsyncIterable

from dishka import AsyncContainer, Provider, Scope, make_async_container, provide
from sqlalchemy.ext.asyncio import AsyncEngine, AsyncSession, async_sessionmaker

from botka.config import Settings
from botka.db.session import create_engine, create_sessionmaker
from botka.services.borrowed_item_detector import BorrowedItemDetector
from botka.services.borrowed_items_service import BorrowedItemsService
from botka.services.fridge_client import FridgeClient
from botka.services.mac_tracker_service import MacTrackerService, MikrotikDhcpClient
from botka.services.meeting_service import MeetingService
from botka.services.planka_album_tracker import PlankaAlbumTracker
from botka.services.planka_action_dispatcher import PlankaActionDispatcher
from botka.services.planka_attachment_cache_service import PlankaAttachmentCacheService
from botka.services.planka_client import PlankaClient
from botka.services.planka_command_service import PlankaCommandService
from botka.services.planka_mappings_service import PlankaCardMappingService
from botka.services.planka_notification_service import PlankaNotificationService
from botka.services.planka_todo_publisher import PlankaTodoPublisher
from botka.services.polls_service import PollsService
from botka.services.printer_service import PrinterService
from botka.services.refinance_client import RefinanceClient
from botka.services.shopping_list_service import (
    ShoppingBuyConfirmationTracker,
    ShoppingListService,
)
from botka.services.shopping_needs_publisher import ShoppingNeedsPublisher
from botka.services.ups_client import UpsClient
from botka.services.user_service import UserService
from botka.services.usbutler_service import UsbutlerService
from botka.services.visit_service import VisitService


class AppProvider(Provider):
    def __init__(self, settings: Settings) -> None:
        super().__init__()
        self._settings = settings

    @provide(scope=Scope.APP)
    def settings(self) -> Settings:
        return self._settings

    @provide(scope=Scope.APP)
    def engine(self, settings: Settings) -> AsyncEngine:
        return create_engine(settings.database_url)

    @provide(scope=Scope.APP)
    def sessionmaker(self, engine: AsyncEngine) -> async_sessionmaker:
        return create_sessionmaker(engine)

    @provide(scope=Scope.REQUEST)
    async def session(
        self, sessionmaker: async_sessionmaker
    ) -> AsyncIterable[AsyncSession]:
        async with sessionmaker() as session:
            yield session

    @provide(scope=Scope.REQUEST)
    def user_service(self, session: AsyncSession, settings: Settings) -> UserService:
        return UserService(session, settings)

    @provide(scope=Scope.REQUEST)
    def visit_service(self, session: AsyncSession) -> VisitService:
        return VisitService(session)

    @provide(scope=Scope.REQUEST)
    def shopping_service(self, session: AsyncSession) -> ShoppingListService:
        return ShoppingListService(session)

    @provide(scope=Scope.APP)
    def shopping_confirmation_tracker(self) -> ShoppingBuyConfirmationTracker:
        return ShoppingBuyConfirmationTracker()

    @provide(scope=Scope.APP)
    def shopping_needs_publisher(
        self,
        sessionmaker: async_sessionmaker,
        settings: Settings,
    ) -> ShoppingNeedsPublisher:
        return ShoppingNeedsPublisher(sessionmaker, settings)

    @provide(scope=Scope.REQUEST)
    def borrowed_items_service(self, session: AsyncSession) -> BorrowedItemsService:
        return BorrowedItemsService(session)

    @provide(scope=Scope.REQUEST)
    def polls_service(self, session: AsyncSession) -> PollsService:
        return PollsService(session)

    @provide(scope=Scope.REQUEST)
    def meeting_service(self, session: AsyncSession) -> MeetingService:
        return MeetingService(session)

    @provide(scope=Scope.APP)
    def mikrotik_client(self, settings: Settings) -> MikrotikDhcpClient:
        return MikrotikDhcpClient(settings)

    @provide(scope=Scope.REQUEST)
    def mac_tracker_service(
        self,
        session: AsyncSession,
        settings: Settings,
        mikrotik_client: MikrotikDhcpClient,
    ) -> MacTrackerService:
        return MacTrackerService(session, settings, mikrotik_client)

    @provide(scope=Scope.APP)
    def borrowed_item_detector(self, settings: Settings) -> BorrowedItemDetector:
        return BorrowedItemDetector(settings)

    @provide(scope=Scope.APP)
    def usbutler_service(self, settings: Settings) -> UsbutlerService:
        return UsbutlerService(settings)

    @provide(scope=Scope.APP)
    def ups_client(self, settings: Settings) -> UpsClient:
        return UpsClient(settings)

    @provide(scope=Scope.APP)
    async def planka_client(self, settings: Settings) -> AsyncIterable[PlankaClient]:
        client = PlankaClient(
            base_url=settings.planka_base_url or "",
            username_or_email=settings.planka_username_or_email or "",
            password=settings.planka_password or "",
            timeout_seconds=settings.planka_request_timeout_seconds,
        )
        if client.is_configured:
            try:
                await client.start()
            except Exception as exc:
                logging.getLogger(__name__).warning(
                    "Planka unavailable, integration disabled: %s", exc
                )
        yield client
        if client.is_configured:
            await client.close()

    @provide(scope=Scope.APP)
    def fridge_client(self, settings: Settings) -> FridgeClient:
        return FridgeClient(settings)

    @provide(scope=Scope.APP)
    async def refinance_client(
        self, settings: Settings
    ) -> AsyncIterable[RefinanceClient]:
        client = RefinanceClient(settings)
        try:
            if client.is_configured:
                await client.verify_bot_entity()
            yield client
        finally:
            await client.close()

    @provide(scope=Scope.APP)
    def planka_album_tracker(self) -> PlankaAlbumTracker:
        return PlankaAlbumTracker()

    @provide(scope=Scope.APP)
    def planka_notification_service(
        self, settings: Settings
    ) -> PlankaNotificationService:
        return PlankaNotificationService(settings)

    @provide(scope=Scope.REQUEST)
    def planka_mappings_service(
        self, session: AsyncSession
    ) -> PlankaCardMappingService:
        return PlankaCardMappingService(session)

    @provide(scope=Scope.REQUEST)
    def planka_attachment_cache_service(
        self, session: AsyncSession
    ) -> PlankaAttachmentCacheService:
        return PlankaAttachmentCacheService(session)

    @provide(scope=Scope.APP)
    def planka_todo_publisher(
        self,
        sessionmaker: async_sessionmaker,
        settings: Settings,
    ) -> PlankaTodoPublisher:
        return PlankaTodoPublisher(sessionmaker, settings)

    @provide(scope=Scope.APP)
    async def planka_action_dispatcher(
        self,
        sessionmaker: async_sessionmaker,
        settings: Settings,
        planka: PlankaClient,
        tracker: PlankaAlbumTracker,
        notifications: PlankaNotificationService,
        todo_publisher: PlankaTodoPublisher,
    ) -> AsyncIterable[PlankaActionDispatcher]:
        dispatcher = PlankaActionDispatcher(
            sessionmaker,
            settings,
            planka,
            tracker,
            notifications,
            todo_publisher,
        )
        yield dispatcher
        await dispatcher.close()

    @provide(scope=Scope.REQUEST)
    def planka_command_service(
        self,
        planka: PlankaClient,
        mappings: PlankaCardMappingService,
        settings: Settings,
        tracker: PlankaAlbumTracker,
    ) -> PlankaCommandService:
        return PlankaCommandService(planka, mappings, settings, tracker)

    @provide(scope=Scope.APP)
    async def printer_service(self, settings: Settings) -> AsyncIterable[PrinterService]:
        service = PrinterService.from_settings(settings)
        await service.start()
        yield service
        await service.close()


def build_container(settings: Settings) -> AsyncContainer:
    return make_async_container(AppProvider(settings))
