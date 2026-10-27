package com.bongocat.catscreen;

import java.io.IOException;
import java.nio.charset.StandardCharsets;

/**
 * The strip under the picture, described by the publisher.
 *
 * <p>Lines carry templates, not finished strings: the device substitutes clock
 * placeholders from its own wall clock every time it repaints, so a ticking
 * minute costs no bandwidth and the clock keeps running while the link is down.
 * Colours are {@code 0xAARRGGBB} ints, which is exactly what an Android
 * {@code Color} int is, so no channel is reordered on either side of the wire.
 */
final class Overlay {

    static final int MAX_LINES = 8;
    static final int MAX_TEXT_BYTES = 256;

    static final int ALIGN_LEFT = 0;
    static final int ALIGN_CENTER = 1;
    static final int ALIGN_RIGHT = 2;

    static final int WEIGHT_REGULAR = 0;
    static final int WEIGHT_BOLD = 1;

    static final int FONT_SYSTEM = 0;
    static final int FONT_ROUNDED = 1;
    static final int FONT_MONO = 2;

    /** One row of text inside the strip. */
    static final class Line {

        final String text;
        final int sizePx;
        final int color;
        final int align;
        final int weight;
        final int font;
        /** Centre of the text, in pixels below the top of the strip. */
        final float centerY;

        Line(String text, int sizePx, int color, int align, int weight, int font, float centerY) {
            this.text = text;
            this.sizePx = sizePx;
            this.color = color;
            this.align = align;
            this.weight = weight;
            this.font = font;
            this.centerY = centerY;
        }
    }

    /** Strip height in pixels. Zero means the strip is off. */
    final int bandPx;
    final int background;
    final boolean divider;
    final Line[] lines;

    Overlay(int bandPx, int background, boolean divider, Line[] lines) {
        this.bandPx = bandPx;
        this.background = background;
        this.divider = divider;
        this.lines = lines;
    }

    /**
     * Decodes one {@code KIND_OVERLAY} payload. The reader is positioned after
     * the kind and length; the payload starts with the protocol version.
     */
    static Overlay read(Protocol.Reader reader) throws IOException {
        if (reader.u32() != Protocol.VERSION) {
            throw new IOException("overlay carries an unknown version");
        }
        int bandPx = reader.u32();
        int background = reader.u32();
        boolean divider = reader.u8() != 0;
        reader.u8();
        reader.u16();

        int count = reader.bounded(MAX_LINES);
        Line[] lines = new Line[count];
        for (int i = 0; i < count; i++) {
            int align = reader.u8();
            int weight = reader.u8();
            int font = reader.u8();
            reader.u8();
            int sizePx = reader.u16();
            reader.u16();
            int color = reader.u32();
            float centerY = reader.f32();
            int length = reader.bounded(MAX_TEXT_BYTES);
            String text = new String(reader.buffer(), reader.position(), length,
                    StandardCharsets.UTF_8);
            reader.advance(length);
            lines[i] = new Line(text, sizePx, color, align, weight, font, centerY);
        }
        return new Overlay(bandPx, background, divider, lines);
    }
}
