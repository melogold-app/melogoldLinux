#version 300 es
// Фон «Сейчас играет» (задание 0032): жидкая обложка + лава-лампа + пульс на бас. Эталон для всех платформ.
precision highp float;
in vec2 vUv;
out vec4 frag;
uniform sampler2D uCover;
uniform vec2 uRes;
uniform float uLiquid, uLiquidFlow, uLavaFlow, uPulse, uLavaMix, uLoud, uLuma;
uniform float uLiquidBase, uBlobScale, uZoom, uLodBase, uLodPulse, uBright, uSat;
uniform float uBlob[6];
uniform vec3 uPal[6];
uniform float uShare[6];
uniform int uPalCount;

vec2 rot(vec2 p, float a) { float c = cos(a), s = sin(a); return mat2(c, s, -s, c) * p; }

vec3 blurAt(vec2 uv, float lod) {
    uv.y = 1.0 - uv.y;
    float r = exp2(lod) / 256.0 * 1.4;
    vec3 c = textureLod(uCover, uv, lod).rgb * 0.36;
    for (int i = 0; i < 4; i++) {
        float a = float(i) * 1.5708 + 0.785;
        c += textureLod(uCover, uv + r * vec2(cos(a), sin(a)), lod).rgb * 0.16;
    }
    return c;
}

void main() {
    float aspect = uRes.x / uRes.y;
    float pulse = clamp(uPulse, 0.0, 1.0);
    // Пульс на бас: удар чуть приближает фон, собирает размытие и поднимает свет.
    // Приближение — едва (0,02): на 0,06 оно бросалось в глаза (Максим, 18.10.2026).
    vec2 p = (vUv - 0.5) * vec2(aspect, 1.0) * (1.0 - uZoom * pulse);
    float lod = mix(uLodBase, uLodPulse, pulse);

    // Жидкая обложка: течение искажает само себя; сила и скорость — от своей полосы.
    vec2 q = rot(p, uLiquidFlow * 0.045);
    vec2 q2 = rot(p * 0.82, -uLiquidFlow * 0.065 + 1.5708) + 0.18 * vec2(sin(uLiquidFlow * 0.059), cos(uLiquidFlow * 0.043));
    float amp = uLiquidBase + 0.62 * uLiquid;
    for (int i = 0; i < 3; i++) {
        float fi = float(i);
        q += amp * 0.32 * vec2(sin(q.y * 2.3 + uLiquidFlow * 0.7 + fi * 1.3), cos(q.x * 2.0 - uLiquidFlow * 0.55 + fi * 2.1));
        q2 += amp * 0.28 * vec2(cos(q2.y * 1.9 - uLiquidFlow * 0.6 + fi), sin(q2.x * 2.4 + uLiquidFlow * 0.5 + fi * 1.7));
    }
    vec3 col = mix(blurAt(0.5 + q * 0.62, lod), blurAt(0.5 + q2 * 0.62, lod + 0.5), 0.38);

    // Лава-лампа: капли цветов обложки, размер — доля цвета и своя сила звука.
    vec3 acc = vec3(0.0);
    float field = 0.0;
    for (int i = 0; i < 6; i++) {
        if (i >= uPalCount) break;
        float fi = float(i);
        float rad = (0.13 + 0.48 * sqrt(uShare[i])) * uBlobScale * (1.0 + 0.55 * uBlob[i]);
        vec2 c = vec2(sin(uLavaFlow * 0.21 + fi * 2.4) * aspect * 0.42, cos(uLavaFlow * 0.17 + fi * 1.7) * 0.42);
        c += 0.07 * vec2(sin(uLavaFlow * 0.9 + fi), cos(uLavaFlow * 1.1 + fi * 2.0));
        float f = rad * rad / (dot(p - c, p - c) + 0.003);
        field += f;
        acc += uPal[i] * f;
    }
    vec3 lavaCol = acc / max(field, 1e-3);
    float m = smoothstep(0.7, 1.8, field);
    col = mix(col * 0.82, lavaCol, m * uLavaMix + 0.08);

    float k = clamp(0.30 / max(uLuma, 0.01), 0.30, 1.0) * uBright * (1.0 + 0.16 * pulse + 0.08 * uLoud);
    float l = dot(col, vec3(0.2126, 0.7152, 0.0722));
    col = mix(vec3(l), col, uSat) * k;
    float v = smoothstep(0.55, 1.0, length((vUv - 0.5) / 0.75));
    col *= 1.0 - 0.3 * v;
    frag = vec4(clamp(col, 0.0, 1.0), 1.0);
}
