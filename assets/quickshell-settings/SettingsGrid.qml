import QtQuick
import QtQuick.Layouts

GridLayout {
    id: root

    property int collapseWidth: 620

    Layout.fillWidth: true
    columns: width >= collapseWidth ? 2 : 1
    columnSpacing: 10
    rowSpacing: 10
}
