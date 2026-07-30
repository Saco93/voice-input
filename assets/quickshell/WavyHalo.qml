import QtQuick

ShaderEffect {
    id: root

    required property real capsuleWidth
    required property real capsuleHeight
    required property real cornerRadius
    required property real level
    required property real pitch
    required property real timbre
    required property real phase
    required property real strength
    required property color haloColor
    required property vector4d spectrum0
    required property vector4d spectrum1
    required property vector4d spectrum2
    required property real spectralFlux
    required property real spectralCentroid
    required property real breath
    required property real stage
    readonly property real haloPadding: 54
    readonly property real effectWidth: width
    readonly property real effectHeight: height

    width: capsuleWidth + 2 * haloPadding
    height: capsuleHeight + 2 * haloPadding
    blending: true
    fragmentShader: Qt.resolvedUrl("shaders/wavy-halo.frag.qsb")
}
