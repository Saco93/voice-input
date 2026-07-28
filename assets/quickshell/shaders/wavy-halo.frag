#version 440

layout(location = 0) in vec2 qt_TexCoord0;
layout(location = 0) out vec4 fragColor;

layout(std140, binding = 0) uniform buf {
    mat4 qt_Matrix;
    float qt_Opacity;
    float effectWidth;
    float effectHeight;
    float capsuleWidth;
    float capsuleHeight;
    float cornerRadius;
    float level;
    float pitch;
    float timbre;
    float phase;
    float strength;
    vec4 haloColor;
    vec4 spectrum0;
    vec4 spectrum1;
    vec4 spectrum2;
    float spectralFlux;
    float spectralCentroid;
    float breath;
};

float roundedBoxDistance(vec2 point, vec2 halfSize, float radius)
{
    vec2 q = abs(point) - (halfSize - vec2(radius));
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

float spectrumBand(float index)
{
    float wrapped = mod(index + 12.0, 12.0);
    if (wrapped < 1.0)
        return spectrum0.x;
    if (wrapped < 2.0)
        return spectrum0.y;
    if (wrapped < 3.0)
        return spectrum0.z;
    if (wrapped < 4.0)
        return spectrum0.w;
    if (wrapped < 5.0)
        return spectrum1.x;
    if (wrapped < 6.0)
        return spectrum1.y;
    if (wrapped < 7.0)
        return spectrum1.z;
    if (wrapped < 8.0)
        return spectrum1.w;
    if (wrapped < 9.0)
        return spectrum2.x;
    if (wrapped < 10.0)
        return spectrum2.y;
    if (wrapped < 11.0)
        return spectrum2.z;
    return spectrum2.w;
}

// A cubic B-spline turns the live band measurements into one smooth spectral
// envelope. It does not impose a visual peak count: local maxima and plateaus
// emerge from the current audio values themselves.
float spectrumEnvelope(float coordinate)
{
    float position = fract(coordinate) * 12.0;
    float base = floor(position);
    float t = fract(position);
    float t2 = t * t;
    float t3 = t2 * t;
    float weight0 = (1.0 - 3.0 * t + 3.0 * t2 - t3) / 6.0;
    float weight1 = (4.0 - 6.0 * t2 + 3.0 * t3) / 6.0;
    float weight2 = (1.0 + 3.0 * t + 3.0 * t2 - 3.0 * t3) / 6.0;
    float weight3 = t3 / 6.0;
    return clamp(
        weight0 * spectrumBand(base - 1.0)
            + weight1 * spectrumBand(base)
            + weight2 * spectrumBand(base + 1.0)
            + weight3 * spectrumBand(base + 2.0),
        0.0,
        1.0
    );
}

// Map each exterior point to its closest position on the rounded capsule.
// Straight-edge coordinates are independent of outward distance, preventing
// the diagonal shear produced by projecting rays from the capsule center.
float perimeterCoordinate(vec2 point, vec2 halfSize, float radius)
{
    vec2 inner = max(halfSize - vec2(radius), vec2(0.0));
    vec2 delta = abs(point) - inner;
    float straightWidth = 2.0 * inner.x;
    float straightHeight = 2.0 * inner.y;
    float quarterArc = 1.5707963267948966 * radius;
    float perimeterLength = 2.0 * (straightWidth + straightHeight) + 4.0 * quarterArc;
    float distanceAlongEdge;

    if (delta.x > 0.0 && delta.y > 0.0) {
        vec2 center = vec2(point.x < 0.0 ? -inner.x : inner.x,
            point.y < 0.0 ? -inner.y : inner.y);
        float angle = atan(point.y - center.y, point.x - center.x);
        if (point.x >= 0.0 && point.y < 0.0) {
            distanceAlongEdge = straightWidth + radius * (angle + 1.5707963267948966);
        } else if (point.x >= 0.0) {
            distanceAlongEdge = straightWidth + quarterArc + straightHeight
                + radius * angle;
        } else if (point.y >= 0.0) {
            distanceAlongEdge = 2.0 * straightWidth + 2.0 * quarterArc + straightHeight
                + radius * (angle - 1.5707963267948966);
        } else {
            if (angle < 0.0)
                angle += 6.283185307179586;
            distanceAlongEdge = 2.0 * straightWidth + 3.0 * quarterArc
                + 2.0 * straightHeight + radius * (angle - 3.141592653589793);
        }
    } else if (delta.y >= delta.x) {
        if (point.y < 0.0)
            distanceAlongEdge = clamp(point.x + inner.x, 0.0, straightWidth);
        else
            distanceAlongEdge = straightWidth + 2.0 * quarterArc + straightHeight
                + clamp(inner.x - point.x, 0.0, straightWidth);
    } else if (point.x >= 0.0) {
        distanceAlongEdge = straightWidth + quarterArc
            + clamp(point.y + inner.y, 0.0, straightHeight);
    } else {
        distanceAlongEdge = 2.0 * straightWidth + 3.0 * quarterArc + straightHeight
            + clamp(inner.y - point.y, 0.0, straightHeight);
    }

    return distanceAlongEdge / max(perimeterLength, 1.0);
}

void main()
{
    vec2 effectSize = max(vec2(effectWidth, effectHeight), vec2(1.0));
    vec2 halfCapsule = max(0.5 * vec2(capsuleWidth, capsuleHeight), vec2(1.0));
    float radius = clamp(cornerRadius, 0.0, min(halfCapsule.x, halfCapsule.y));
    vec2 point = qt_TexCoord0 * effectSize - 0.5 * effectSize;
    float distanceToCapsule = roundedBoxDistance(point, halfCapsule, radius);

    vec2 normalizedPoint = point / halfCapsule;
    float straightWidth = max(capsuleWidth - 2.0 * radius, 0.0);
    float straightHeight = max(capsuleHeight - 2.0 * radius, 0.0);
    float perimeterLength = 2.0 * (straightWidth + straightHeight)
        + 6.283185307179586 * radius;
    float perimeter = perimeterCoordinate(point, halfCapsule, radius);
    float theta = perimeter * 6.28318530717958647692;
    float travelAngle = phase * 6.28318530717958647692 / max(perimeterLength, 1.0);
    float travelingTheta = theta - travelAngle;

    float activity = clamp(level, 0.0, 1.0);
    float normalizedPitch = clamp(pitch, 0.0, 1.0);
    float normalizedTimbre = clamp(timbre, 0.0, 1.0);
    float normalizedFlux = clamp(spectralFlux, 0.0, 1.0);
    float normalizedCentroid = clamp(spectralCentroid, 0.0, 1.0);
    float normalizedBreath = clamp(breath, 0.0, 1.0);

    // Every component uses the same traveling coordinate, so changing pitch
    // changes spatial density without changing the wave's border speed.
    float mode = mix(5.0, 12.0, normalizedPitch);
    float lowerMode = floor(mode);
    float modeBlend = smoothstep(0.0, 1.0, fract(mode));
    float fundamental = mix(
        sin(lowerMode * travelingTheta),
        sin((lowerMode + 1.0) * travelingTheta),
        modeBlend
    );

    // Adjacent-mode interference and a broad low-frequency envelope stop the
    // peaks from looking mechanically equal. Timbre raises the contribution of
    // the finer components, approximating the measured high-band energy.
    float interferenceWeight = mix(0.08, 0.18, normalizedTimbre);
    float interference = mix(
        sin((lowerMode + 2.0) * travelingTheta + 1.7),
        sin((lowerMode + 3.0) * travelingTheta + 1.7),
        modeBlend
    );
    float harmonicWeight = mix(0.04, 0.12, normalizedTimbre);
    float harmonic = mix(
        sin(2.0 * lowerMode * travelingTheta + 0.8),
        sin(2.0 * (lowerMode + 1.0) * travelingTheta + 0.8),
        modeBlend
    );
    float profile = (fundamental + interferenceWeight * interference
        + harmonicWeight * harmonic)
        / (1.0 + interferenceWeight + harmonicWeight);
    float heightEnvelope = clamp(
        0.82
            + 0.10 * sin(3.0 * travelingTheta + 1.1 + normalizedTimbre)
            + 0.05 * sin(5.0 * travelingTheta - 1.4 + normalizedPitch),
        0.65,
        1.0
    );
    float verticalDirection = -normalizedPoint.y / max(length(normalizedPoint), 0.00001);
    float directionalBias = 0.20 * (2.0 * normalizedTimbre - 1.0) * verticalDirection;
    float wave = clamp(0.5 + 0.5 * profile + directionalBias, 0.0, 1.0);
    float shapedWave = smoothstep(0.04, 0.96, wave);
    float heightWave = clamp(pow(shapedWave, 0.82) * heightEnvelope, 0.0, 1.0);

    // The twelve log-frequency bands replace the evenly spaced synthetic wave
    // during speech. Quiet bands leave flat regions; energetic bands create
    // wider local peaks, preserving the actual spectral envelope as it travels.
    float spectralCoordinate = fract(perimeter - phase / max(perimeterLength, 1.0));
    float band0 = clamp(spectrum0.x, 0.0, 1.0);
    float band1 = clamp(spectrum0.y, 0.0, 1.0);
    float band2 = clamp(spectrum0.z, 0.0, 1.0);
    float band3 = clamp(spectrum0.w, 0.0, 1.0);
    float band4 = clamp(spectrum1.x, 0.0, 1.0);
    float band5 = clamp(spectrum1.y, 0.0, 1.0);
    float band6 = clamp(spectrum1.z, 0.0, 1.0);
    float band7 = clamp(spectrum1.w, 0.0, 1.0);
    float band8 = clamp(spectrum2.x, 0.0, 1.0);
    float band9 = clamp(spectrum2.y, 0.0, 1.0);
    float band10 = clamp(spectrum2.z, 0.0, 1.0);
    float band11 = clamp(spectrum2.w, 0.0, 1.0);
    float spectralWave = smoothstep(0.06, 0.88, spectrumEnvelope(spectralCoordinate));
    float spectralPresence = max(max(max(band0, band1), max(band2, band3)),
        max(max(max(band4, band5), max(band6, band7)),
            max(max(band8, band9), max(band10, band11))));
    float spectralBlend = activity * smoothstep(0.02, 0.25, spectralPresence) * 0.92;
    heightWave = mix(heightWave, spectralWave, spectralBlend);

    // The halo always occupies a visible breathing radius. Speech can only
    // expand from this baseline; the spectrum shapes the additional reach but
    // never switches the glow between absent and present.
    float ambientReach = mix(8.0, 15.0, normalizedBreath) + 2.0 * heightWave;
    float reachPulse = clamp(0.55 * activity + 0.75 * normalizedFlux, 0.0, 1.0);
    float maximumActiveReach = max(
        ambientReach + 3.0,
        mix(24.0, 20.0, normalizedPitch) * mix(0.78, 1.0, reachPulse)
    );
    float activeReach = mix(ambientReach + 2.0, maximumActiveReach, heightWave);
    float localReach = mix(ambientReach, activeReach, activity);
    float antialias = max(fwidth(distanceToCapsule), 0.5);
    float outside = smoothstep(-antialias, antialias, distanceToCapsule);
    float falloff = 1.0 - smoothstep(0.0, localReach, distanceToCapsule);
    falloff *= falloff;

    float ambientContrast = mix(0.52, 1.0, heightWave);
    float activeContrast = mix(0.08, 1.0, pow(heightWave, 0.7));
    float normalizedStrength = clamp(strength, 0.0, 1.0);
    float brightness = normalizedStrength
        * mix(0.85, 1.0, activity)
        * mix(ambientContrast, activeContrast, activity)
        * mix(0.90, 1.0, normalizedFlux);
    float waveAlpha = outside * falloff * brightness * haloColor.a;

    // A narrow continuous foot keeps every moving lobe optically attached to
    // the capsule even when the local wave contrast reaches a deep trough.
    float anchorReach = mix(6.0, 10.0, normalizedBreath) + activity;
    float anchorFalloff = 1.0 - smoothstep(0.0, anchorReach, distanceToCapsule);
    anchorFalloff *= anchorFalloff;
    float anchorAlpha = outside * anchorFalloff * normalizedStrength
        * mix(0.42, 0.58, activity) * haloColor.a;
    float alpha = max(waveAlpha, anchorAlpha);

    // Bright spectral content adds a pale highlight to active peaks while the
    // QML halo color continues to shift hue with the smoothed timbre estimate.
    float spectralTone = mix(normalizedTimbre, normalizedCentroid, 0.55);
    float spectralHighlight = activity * spectralTone * pow(heightWave, 0.6)
        + 0.08 * normalizedFlux;
    vec3 localColor = haloColor.rgb * mix(0.84, 1.0, heightWave);
    localColor = mix(localColor, vec3(1.0), 0.18 * spectralHighlight);
    fragColor = vec4(localColor * alpha, alpha) * qt_Opacity;
}
