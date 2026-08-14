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
    vec4 previousHaloColor;
    float colorTransition;
    float processingBlend;
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

float broadBandPeak(float coordinate, float center, float width)
{
    float distanceFromCenter = coordinate - center;
    return exp(-width * distanceFromCenter * distanceFromCenter);
}

// Arming, silent Listening, and processing supply calm virtual frequency bands
// to the same spline used by microphone input. Their broad peaks preserve full
// geometric reach without pretending that silence or processing is live speech.
float syntheticSpectrumBand(
    float index,
    float primaryCenter,
    float secondaryCenter,
    float primaryWidth,
    float secondaryWidth,
    float primaryWeight
)
{
    float coordinate = clamp(index, 0.0, 11.0) / 11.0;
    float profile = primaryWeight * broadBandPeak(coordinate, primaryCenter, primaryWidth)
        + (1.0 - primaryWeight)
            * broadBandPeak(coordinate, secondaryCenter, secondaryWidth);
    return clamp(0.34 + 0.66 * profile, 0.0, 1.0);
}

float syntheticSpectrumEnvelope(float coordinate, float syntheticStage, float time)
{
    float primaryCenter;
    float secondaryCenter;
    float primaryWidth;
    float secondaryWidth;
    float primaryWeight;
    if (syntheticStage < 0.5) {
        primaryCenter = 0.50 + 0.08 * sin(6.2831853072 * time * 0.70);
        secondaryCenter = 0.24 + 0.04 * sin(6.2831853072 * time * 0.46 + 1.2);
        primaryWidth = 7.0;
        secondaryWidth = 11.0;
        primaryWeight = 0.80;
    } else {
        // Finalizing, Refining, and Sending share this one continuously moving
        // profile. Stage changes therefore alter color without selecting a new
        // geometric animation or restarting its phase.
        primaryCenter = 0.50 + 0.12 * sin(6.2831853072 * time * 0.78);
        secondaryCenter = 0.72 - 0.06 * sin(6.2831853072 * time * 0.52 + 0.8);
        primaryWidth = 6.0;
        secondaryWidth = 9.0;
        primaryWeight = 0.76;
    }

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
        weight0 * syntheticSpectrumBand(base - 1.0, primaryCenter,
            secondaryCenter, primaryWidth, secondaryWidth, primaryWeight)
            + weight1 * syntheticSpectrumBand(base, primaryCenter,
                secondaryCenter, primaryWidth, secondaryWidth, primaryWeight)
            + weight2 * syntheticSpectrumBand(base + 1.0, primaryCenter,
                secondaryCenter, primaryWidth, secondaryWidth, primaryWeight)
            + weight3 * syntheticSpectrumBand(base + 2.0, primaryCenter,
                secondaryCenter, primaryWidth, secondaryWidth, primaryWeight),
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
    float antialias = max(fwidth(distanceToCapsule), 0.5);
    if (distanceToCapsule <= -antialias) {
        fragColor = vec4(0.0);
        return;
    }

    float straightWidth = max(capsuleWidth - 2.0 * radius, 0.0);
    float straightHeight = max(capsuleHeight - 2.0 * radius, 0.0);
    float perimeterLength = 2.0 * (straightWidth + straightHeight)
        + 6.283185307179586 * radius;
    float perimeter = perimeterCoordinate(point, halfCapsule, radius);

    float activity = clamp(level, 0.0, 1.0);
    float normalizedPitch = clamp(pitch, 0.0, 1.0);
    float normalizedTimbre = clamp(timbre, 0.0, 1.0);
    float normalizedFlux = clamp(spectralFlux, 0.0, 1.0);
    float normalizedCentroid = clamp(spectralCentroid, 0.0, 1.0);
    float normalizedBreath = clamp(breath, 0.0, 1.0);

    // Stage-specific inputs assign this below. Keeping the legacy perimeter
    // wave out of the Listening path prevents it from resurfacing while live
    // speech decays into the quiet standby envelope.
    float heightWave = 0.0;

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
    float syntheticTime = phase / max(capsuleWidth, 1.0);
    float syntheticEnvelope = syntheticSpectrumEnvelope(
        spectralCoordinate,
        stage,
        syntheticTime
    );
    float syntheticWave = smoothstep(0.34, 0.88, syntheticEnvelope);
    syntheticWave = pow(syntheticWave, 0.90);
    float spectralPresence = max(max(max(band0, band1), max(band2, band3)),
        max(max(max(band4, band5), max(band6, band7)),
            max(max(band8, band9), max(band10, band11))));
    // `activity` is already a calibrated nonlinear voice estimate. Turn it
    // into a presence gate once instead of multiplying the spectrum and reach
    // by the same small value twice, which flattened ordinary speech.
    float speechPresence = smoothstep(0.015, 0.18, activity);
    float spectralAvailability = smoothstep(0.02, 0.25, spectralPresence) * 0.98;

    // Every active stage uses a frequency envelope on the same top-edge
    // renderer. Listening keeps the live microphone spectrum. The complete
    // post-recording pipeline shares one synthetic profile and one continuous
    // phase, while processingBlend softens the handoff from the final live frame.
    float topTravel = phase / max(straightWidth, 1.0);
    // Silent Listening uses the same full-height virtual spectrum as processing.
    // Speech replaces that calm source with measured bands without changing the
    // renderer or inserting the retired low-amplitude perimeter ripple.
    float standbyHeight = syntheticWave;
    float liveHeight = mix(standbyHeight, spectralWave, spectralAvailability);
    float listeningHeight = mix(standbyHeight, liveHeight, speechPresence);
    float listeningDrive = mix(0.90, 1.0, speechPresence);
    float stageDrive = speechPresence;
    if (stage < 0.5) {
        heightWave = syntheticWave;
        stageDrive = 0.44;
    } else if (stage < 1.5) {
        heightWave = listeningHeight;
        stageDrive = listeningDrive;
    } else {
        float processingMix = smootherStep(0.0, 1.0, processingBlend);
        heightWave = mix(listeningHeight, syntheticWave, processingMix);
        stageDrive = mix(listeningDrive, 0.68, processingMix);
    }

    // Keep only a restrained contact glow around the complete capsule. The
    // measured top-edge profile carries the visible reach and hierarchy.
    float ambientReach = mix(3.5, 6.0, normalizedBreath);
    float reachPulse = clamp(0.55 * activity + 0.75 * normalizedFlux, 0.0, 1.0);
    float recordingStage = 1.0 - smoothstep(0.20, 0.45, abs(stage - 1.0));
    float silentListening = recordingStage * (1.0 - speechPresence);
    float syntheticStage = step(-0.5, stage) * (1.0 - recordingStage);
    float fullHeightVirtual = max(syntheticStage, silentListening);
    float reachDrive = mix(stageDrive, 1.0, fullHeightVirtual);
    reachPulse = max(reachPulse, fullHeightVirtual);
    float maximumActiveReach = max(
        ambientReach + 3.0,
        mix(50.0, 42.0, normalizedPitch) * mix(0.84, 1.0, reachPulse)
    );
    // Taper geometric height itself toward the ambient Anchor Glow. Fading
    // only opacity leaves a tall translucent wall at each endpoint.
    float taperedHeightWave = heightWave * topEdgeMask;
    float activeReach = mix(ambientReach, maximumActiveReach, taperedHeightWave);
    float topSpectrumDrive = topEdgeMask * stageDrive;
    float localReach = mix(ambientReach, activeReach, reachDrive);
    float outside = smoothstep(-antialias, antialias, distanceToCapsule);
    float falloff = 1.0 - smoothstep(0.0, localReach, distanceToCapsule);
    falloff *= falloff;

    // Keep troughs visible enough to read as low waveform regions instead of
    // flicker, while peak height carries most of the spectral contrast.
    float activeContrast = mix(0.18, 1.0, pow(heightWave, 0.7));
    float normalizedStrength = clamp(strength, 0.0, 1.0);
    float ambientBrightness = normalizedStrength
        * mix(0.16, 0.25, normalizedBreath);
    float activeBrightness = normalizedStrength
        * mix(0.85, 1.0, activity)
        * activeContrast
        * mix(0.90, 1.0, normalizedFlux);
    float brightness = mix(ambientBrightness, activeBrightness, topSpectrumDrive);
    // Blend the complete halo at once. A spatial wipe introduced a visible
    // vertical boundary where old and new phase colors met on the top edge.
    float colorBlend = smootherStep(0.0, 1.0, colorTransition);
    vec4 effectiveHaloColor = mix(previousHaloColor, haloColor, colorBlend);
    float sourceAlpha = effectiveHaloColor.a;
    vec3 straightHaloColor = sourceAlpha > 0.0
        ? effectiveHaloColor.rgb / sourceAlpha
        : vec3(0.0);
    float waveAlpha = outside * falloff * brightness * sourceAlpha;

    // A narrow continuous foot keeps every spectral lobe optically attached to
    // the capsule even when the local wave contrast reaches a deep trough.
    float anchorReach = mix(3.0, 5.0, normalizedBreath) + activity;
    float anchorFalloff = 1.0 - smoothstep(0.0, anchorReach, distanceToCapsule);
    anchorFalloff *= anchorFalloff;
    float anchorAlpha = outside * anchorFalloff * normalizedStrength
        * mix(0.16, 0.24, activity) * sourceAlpha;
    float alpha = max(waveAlpha, anchorAlpha);

    // Enter processing with a short breath-like handoff: preserve the final
    // Listening frame, dip almost out while geometry and color change, then
    // restore the shared processing halo. Later processing phases keep blend=1
    // and therefore do not repeat this visibility transition.
    float handoffProgress = clamp(processingBlend, 0.0, 1.0);
    float distanceFromHandoffCenter = abs(2.0 * handoffProgress - 1.0);
    float handoffVisibility = mix(
        0.05,
        1.0,
        smootherStep(0.0, 1.0, distanceFromHandoffCenter)
    );
    float processingStageMask = step(1.5, stage);
    alpha *= mix(1.0, handoffVisibility, processingStageMask);

    // Frequency position controls local hue while energy controls saturation
    // and lightness. As different bands become dominant, the visible peak
    // colors change even when the speaker's long-term timbre stays constant.
    float spectralTone = mix(normalizedTimbre, normalizedCentroid, 0.82);
    float spectralHighlight = topSpectrumDrive * spectralTone * pow(heightWave, 0.6)
        + 0.08 * normalizedFlux * topEdgeMask;
    vec3 localColor = straightHaloColor * mix(0.84, 1.0, heightWave);
    localColor = mix(localColor, vec3(1.0), 0.14 * spectralHighlight);
    vec3 baseHsv = rgbToHsv(straightHaloColor);
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
