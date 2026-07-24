import QtQuick
import Quickshell

ShellRoot {
    id: shell

    StateStore {
        id: stateStore
    }

    Variants {
        model: Quickshell.screens

        HudSurface {
            required property var modelData
            screenModel: modelData
            store: stateStore
        }
    }
}
