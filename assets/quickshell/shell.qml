import QtQuick
import Quickshell

ShellRoot {
    id: shell

    readonly property string fontPath: Quickshell.env("VOICE_INPUT_FONT_PATH") || ""
    readonly property string fontFamily: uiFont.status === FontLoader.Ready ? uiFont.name : ""

    FontLoader {
        id: uiFont

        source: shell.fontPath.length > 0 ? "file://" + shell.fontPath : ""
        onStatusChanged: {
            if (status === FontLoader.Ready)
                console.log("Voice Input HUD font:", name);
        }
    }

    StateStore {
        id: stateStore
    }

    Variants {
        model: Quickshell.screens

        HudSurface {
            required property ShellScreen modelData
            screenModel: modelData
            store: stateStore
            fontFamily: shell.fontFamily
        }
    }
}
