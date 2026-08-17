from botka.handlers.bambu.utils import camera_keyboard, status_keyboard
from botka.handlers.help.commands import COMMANDS


def test_3d_replaces_bambu_in_registered_commands() -> None:
    command_names = [command.command for command in COMMANDS]

    assert "3d" in command_names
    assert "bambu" not in command_names


def test_printer_keyboards_use_generic_callback_ids() -> None:
    status_callbacks = [
        button.callback_data
        for row in status_keyboard(["Voron"]).inline_keyboard
        for button in row
    ]
    camera_callbacks = [
        button.callback_data
        for row in camera_keyboard("Voron").inline_keyboard
        for button in row
    ]

    assert status_callbacks == ["printer_cam:Voron", "printer_refresh"]
    assert camera_callbacks == ["printer_cam_refresh:Voron"]
