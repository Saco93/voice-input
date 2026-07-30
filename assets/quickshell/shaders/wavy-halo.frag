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
    float stage;
};

vec3 rgbToHsv(vec3 color)
{
    vec4 k = vec4(0.0, -0.3333333333, 0.6666666667, -1.0);
    vec4 p = mix(vec4(color.bg, k.wz), vec4(color.gb, k.xy), step(color.b, color.g));
    vec4 q = mix(vec4(p.xyw, color.r), vec4(color.r, p.yzx), step(p.x, color.r));
    float delta = q.x - min(q.w, q.y);
    float epsilon = 1.0e-10;
    return vec3(abs(q.z + (q.w - q.y) / (6.0 * delta + epsilon)),
        delta / (q.x + epsilon), q.x);
}

vec3 hsvToRgb(vec3 color)
{
    vec3 p = abs(fract(color.xxx + vec3(0.0, 0.6666666667, 0.3333333333)) * 6.0 - 3.0);
    return color.z * mix(vec3(1.0), clamp(p - 1.0, 0.0, 1.0), color.y);
}

float roundedBoxDistance(vec2 point, vec2 halfSize, float radius)
{
    vec2 q = abs(point) - (halfSize - vec2(radius));
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

float smootherStep(float edge0, float edge1, float value)
{
    float t = clamp((value - edge0) / max(edge1 - edge0, 0.00001), 0.0, 1.0);
    return t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
}

float spectrumBand(float index)
{
    float bounded = clamp(index, 0.0, 11.0);
    if (bounded < 1.0)
        return spectrum0.x;
    if (bounded < 2.0)
        return spectrum0.y;
    if (bounded < 3.0)
        return spectrum0.z;
    if (bounded < 4.0)
        return spectrum0.w;
    if (bounded < 5.0)
        return spectrum1.x;
    if (bounded < 6.0)
        return spectrum1.y;
    if (bounded < 7.0)
        return spectrum1.z;
    if (bounded < 8.0)
        return spectrum1.w;
    if (bounded < 9.0)
        return spectrum2.x;
    if (bounded < 10.0)
        return spectrum2.y;
    if (bounded < 11.0)
        return spectrum2.z;
    return spectrum2.w;
}

// A cubic B-spline turns the live band measurements into one smooth spectral
// envelope. It does not impose a visual peak count: local maxima and plateaus
// emerge from the current audio values themselves.
float spectrumEnvelope(float coordinate)
{
    float position = clamp(coordinate, 0.0, 1.0) * 11.0;
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

    // Run the crest through the upper part of both rounded corners as well as
    // the straight top edge. Its normal therefore rotates with the capsule and
    // makes the Anchor Glow look as though it grows naturally out of each arc.
    float quarterArcLength = 1.5707963268 * radius;
    float cornerGrowthLength = 0.72 * quarterArcLength;
    float topPathLength = straightWidth + 2.0 * cornerGrowthLength;
    float perimeterDistance = perimeter * perimeterLength;
    float topPathDistance = -1.0;
    if (perimeterDistance >= perimeterLength - cornerGrowthLength) {
        topPathDistance = perimeterDistance - (perimeterLength - cornerGrowthLength);
    } else if (perimeterDistance <= straightWidth + cornerGrowthLength) {
        topPathDistance = cornerGrowthLength + perimeterDistance;
    }
    float topEdgePosition = topPathDistance / max(topPathLength, 1.0);
    float topPathMask = step(0.0, topPathDistance)
        * step(topPathDistance, topPathLength);
    // Give the complete arc-to-arc path a pixel-bounded quintic taper. Zero
    // first and second derivatives make both endpoint tangencies continuous.
    float endpointRamp = clamp(32.0 / max(topPathLength, 1.0), 0.06, 0.13);
    float topEdgeMask = topPathMask
        * smootherStep(0.0, endpointRamp, topEdgePosition)
        * (1.0 - smootherStep(1.0 - endpointRamp, 1.0, topEdgePosition));
    float spectralCoordinate = clamp(topEdgePosition, 0.0, 1.0);
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
    // Expand the middle of the measured envelope so adjacent quiet and
    // energetic bands produce a visibly deeper geometric height difference.
    float spectralWave = smoothstep(0.48, 0.96, spectrumEnvelope(spectralCoordinate));
    spectralWave = pow(spectralWave, 1.35);
    float spectralPresence = max(max(max(band0, band1), max(band2, band3)),
        max(max(max(band4, band5), max(band6, band7)),
            max(max(band8, band9), max(band10, band11))));
    // `activity` is already a calibrated nonlinear voice estimate. Turn it
    // into a presence gate once instead of multiplying the spectrum and reach
    // by the same small value twice, which flattened ordinary speech.
    float speechPresence = smoothstep(0.015, 0.18, activity);
    float spectralBlend = speechPresence
        * smoothstep(0.02, 0.25, spectralPresence) * 0.98;

    // Every normal pipeline stage uses the same top-edge relief language. The
    // stage only changes the source profile and its emphasis: preparation is
    // sparse, final transcription consolidates, refinement interferes, and
    // output sweeps in one direction. Recording remains the live spectrum.
    float topTravel = phase / max(straightWidth, 1.0);
    float stageDrive = speechPresence;
    if (stage < 0.5) {
        float preparation = 0.5 + 0.5
            * sin(12.5663706144 * spectralCoordinate - 3.2 * topTravel);
        heightWave = 0.16 + 0.24 * preparation * (0.72 + 0.28 * normalizedBreath);
        stageDrive = 0.44;
    } else if (stage < 1.5) {
        // Silent Listening has a restrained traveling ripple instead of a
        // static outline. Live speech smoothly replaces it with the measured
        // frequency envelope as soon as local activity appears.
        float listeningRipple = 0.5 + 0.5
            * sin(12.5663706144 * spectralCoordinate - 2.6 * topTravel);
        float standbyHeight = 0.10 + 0.42 * listeningRipple;
        float liveHeight = mix(heightWave, spectralWave, spectralBlend);
        heightWave = mix(standbyHeight, liveHeight, speechPresence);
        stageDrive = mix(0.90, 1.0, speechPresence);
    } else if (stage < 2.5) {
        float centerDistance = spectralCoordinate - 0.5;
        float consolidation = exp(-14.0 * centerDistance * centerDistance);
        heightWave = 0.16 + 0.48 * consolidation
            * (0.72 + 0.28 * normalizedBreath);
        stageDrive = 0.58;
    } else if (stage < 3.5) {
        float primary = sin(18.8495559215 * spectralCoordinate + 5.0 * topTravel);
        float secondary = sin(31.4159265359 * spectralCoordinate - 3.0 * topTravel + 1.3);
        float interferenceProfile = clamp(0.5 + 0.34 * primary + 0.16 * secondary, 0.0, 1.0);
        heightWave = 0.14 + 0.54 * interferenceProfile;
        stageDrive = 0.70;
    } else {
        float sweepCenter = fract(3.0 * topTravel);
        float sweepDistance = spectralCoordinate - sweepCenter;
        float sweep = exp(-72.0 * sweepDistance * sweepDistance);
        heightWave = 0.10 + 0.72 * sweep;
        stageDrive = 0.82;
    }

    // Keep a small uniform breathing radius around the complete capsule. Only
    // the shared top-edge profile is allowed to expand beyond it.
    float ambientReach = mix(5.0, 9.0, normalizedBreath);
    float reachPulse = clamp(0.55 * activity + 0.75 * normalizedFlux, 0.0, 1.0);
    float maximumActiveReach = max(
        ambientReach + 3.0,
        mix(50.0, 42.0, normalizedPitch) * mix(0.84, 1.0, reachPulse)
    );
    // Taper geometric height itself toward the ambient Anchor Glow. Fading
    // only opacity leaves a tall translucent wall at each endpoint.
    float taperedHeightWave = heightWave * topEdgeMask;
    float activeReach = mix(ambientReach, maximumActiveReach, taperedHeightWave);
    float topSpectrumDrive = topEdgeMask * stageDrive;
    float localReach = mix(ambientReach, activeReach, stageDrive);
    float antialias = max(fwidth(distanceToCapsule), 0.5);
    float outside = smoothstep(-antialias, antialias, distanceToCapsule);
    float falloff = 1.0 - smoothstep(0.0, localReach, distanceToCapsule);
    falloff *= falloff;

    // Keep troughs visible enough to read as low waveform regions instead of
    // flicker, while peak height carries most of the spectral contrast.
    float activeContrast = mix(0.18, 1.0, pow(heightWave, 0.7));
    float normalizedStrength = clamp(strength, 0.0, 1.0);
    float ambientBrightness = normalizedStrength
        * mix(0.28, 0.42, normalizedBreath);
    float activeBrightness = normalizedStrength
        * mix(0.85, 1.0, activity)
        * activeContrast
        * mix(0.90, 1.0, normalizedFlux);
    float brightness = mix(ambientBrightness, activeBrightness, topSpectrumDrive);
    float waveAlpha = outside * falloff * brightness * haloColor.a;

    // A narrow continuous foot keeps every spectral lobe optically attached to
    // the capsule even when the local wave contrast reaches a deep trough.
    float anchorReach = mix(4.0, 7.0, normalizedBreath) + activity;
    float anchorFalloff = 1.0 - smoothstep(0.0, anchorReach, distanceToCapsule);
    anchorFalloff *= anchorFalloff;
    float anchorAlpha = outside * anchorFalloff * normalizedStrength
        * mix(0.24, 0.34, activity) * haloColor.a;
    float alpha = max(waveAlpha, anchorAlpha);
    float recordingStage = 1.0 - smoothstep(0.20, 0.45, abs(stage - 1.0));
    float silentListening = recordingStage * (1.0 - speechPresence);
    float standbyFalloff = 1.0 - smoothstep(0.0, localReach, distanceToCapsule);
    float standbyAlpha = silentListening * topEdgeMask * outside * standbyFalloff
        * mix(0.65, 0.90, normalizedStrength) * mix(0.45, 1.0, heightWave);
    alpha = max(alpha, standbyAlpha);

    // Frequency position controls local hue while energy controls saturation
    // and lightness. As different bands become dominant, the visible peak
    // colors change even when the speaker's long-term timbre stays constant.
    float spectralTone = mix(normalizedTimbre, normalizedCentroid, 0.82);
    float spectralHighlight = topSpectrumDrive * spectralTone * pow(heightWave, 0.6)
        + 0.08 * normalizedFlux * topEdgeMask;
    vec3 localColor = haloColor.rgb * mix(0.84, 1.0, heightWave);
    localColor = mix(localColor, vec3(1.0), 0.14 * spectralHighlight);
    vec3 baseHsv = rgbToHsv(haloColor.rgb);
    float frequencyHue = (spectralCoordinate - 0.5) * 0.28;
    float energyHue = (spectralWave - 0.42) * 0.24
        * smoothstep(0.04, 0.55, spectralPresence);
    float transientHue = normalizedFlux
        * 0.10 * sin(12.5663706144 * spectralCoordinate + 4.0 * topTravel);
    vec3 frequencyColor = hsvToRgb(vec3(
        fract(baseHsv.x + frequencyHue + energyHue
            + (normalizedCentroid - 0.5) * 0.16 + transientHue + 1.0),
        clamp(baseHsv.y + 0.20 * spectralWave, 0.0, 1.0),
        clamp(baseHsv.z + 0.13 * spectralWave, 0.0, 1.0)
    ));
    float frequencyColorDrive = recordingStage * topEdgeMask * speechPresence
        * (0.48 + 0.52 * spectralWave);
    localColor = mix(localColor, frequencyColor, 0.92 * frequencyColorDrive);
    fragColor = vec4(localColor * alpha, alpha) * qt_Opacity;
}
