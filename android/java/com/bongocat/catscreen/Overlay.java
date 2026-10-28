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
    /**
     * Wide enough for the random-banner spec, which carries the enabled pool
     * ids and the person's own sentences after it — a list of short lines, not
     * one sentence, so the old one-sentence bound would cut it in half.
     */
    static final int MAX_TEXT_BYTES = 1024;

    static final int ALIGN_LEFT = 0;
    static final int ALIGN_CENTER = 1;
    static final int ALIGN_RIGHT = 2;
    /**
     * A line that belongs on the clock card rather than on the strip.
     *
     * <p>Its {@code centerY} is then measured from the top of the card, and its
     * x comes from the card's own padding instead of from the circle's chord.
     * The card only exists in the layer layout — a short bar at the bottom of a
     * round panel has no room for one — so the renderer falls back to plain
     * left-aligned text when the band is too short to be a layer.
     */
    static final int ALIGN_CARD = 3;

    static final int WEIGHT_REGULAR = 0;
    static final int WEIGHT_BOLD = 1;

    static final int FONT_SYSTEM = 0;
    static final int FONT_ROUNDED = 1;
    static final int FONT_MONO = 2;
    /**
     * The first font code that names a face this app carries rather than one
     * the firmware ships. `FONT_BUNDLED + n` is the n-th entry of the
     * renderer's list, which is why the two orders have to stay in step.
     */
    static final int FONT_BUNDLED = 3;

    /**
     * How the renderer should break a line that is too wide for its own height
     * of the circle.
     *
     * <p>The break happens here rather than on the host because a random banner
     * is chosen on this side: the host never sees the sentence it would have
     * had to split, and only this side knows how wide the glyphs really are in
     * the face the line is drawn with.
     */
    static final int ROWS_AUTO = 0;
    static final int ROWS_ONE = 1;
    static final int ROWS_TWO = 2;

    /**
     * How the renderer dresses a line.
     *
     * <p>The codes travel in the halfword that used to be padding, so a host
     * that still writes zero asks for plain text, which is what every earlier
     * build drew. Everything past the first few is decorative drawing done
     * around the same rows the plain path lays out.
     */
    static final int STYLE_PLAIN = 0;
    static final int STYLE_PILL = 1;
    static final int STYLE_TWO_TONE = 2;
    static final int STYLE_PAWS = 3;
    static final int STYLE_LOW = 4;
    static final int STYLE_FADE = 5;
    static final int STYLE_BUBBLE = 6;
    static final int STYLE_RAINBOW = 7;
    static final int STYLE_WAVE = 8;
    static final int STYLE_KAOMOJI = 9;
    static final int STYLE_JELLY = 10;
    static final int STYLE_BRASS = 11;
    static final int STYLE_WOOD = 12;
    static final int STYLE_ENAMEL = 13;
    static final int STYLE_PINK_SWEET = 14;
    static final int STYLE_BLACK_GOLD = 15;
    static final int STYLE_TAG = 16;

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
        /** One of the {@code ROWS_*} constants. */
        final int rows;
        /** One of the {@code STYLE_*} constants. */
        final int style;

        Line(String text, int sizePx, int color, int align, int weight, int font, float centerY,
                int rows, int style) {
            this.text = text;
            this.sizePx = sizePx;
            this.color = color;
            this.align = align;
            this.weight = weight;
            this.font = font;
            this.centerY = centerY;
            this.rows = rows;
            this.style = style;
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
            // Was padding until the banner learned to break. A host that still
            // writes zero asks for the behaviour this renderer always had.
            int rows = reader.u8();
            int sizePx = reader.u16();
            // Was padding until the banner learned to dress itself. A host that
            // still writes zero asks for plain text, which is what every
            // earlier build drew.
            int style = reader.u16();
            int color = reader.u32();
            float centerY = reader.f32();
            int length = reader.bounded(MAX_TEXT_BYTES);
            String text = new String(reader.buffer(), reader.position(), length,
                    StandardCharsets.UTF_8);
            reader.advance(length);
            lines[i] = new Line(text, sizePx, color, align, weight, font, centerY, rows, style);
        }
        return new Overlay(bandPx, background, divider, lines);
    }
}
