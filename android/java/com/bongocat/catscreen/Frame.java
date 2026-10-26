package com.bongocat.catscreen;

import java.io.IOException;
import java.util.Arrays;

/** One evaluated frame, parsed from the publisher's {@code KIND_FRAME} message. */
final class Frame {

    static final int MAX_DRAWABLES = 4096;
    static final int MAX_KEYS = 256;
    static final int MAX_MASKS = 64;
    static final int MAX_VERTICES = 65535;
    static final int MAX_INDICES = 65535;

    long generation;
    long sequence;
    float opacity;
    boolean mirror;
    float minX;
    float maxX;
    float minY;
    float maxY;

    int keyCount;
    int[] keyAssetIds = new int[16];
    int[] keySides = new int[16];

    int drawableCount;
    Drawable[] drawables = new Drawable[32];

    static final class Drawable {
        int id;
        int textureId;
        int renderOrder;
        int flags;
        float opacity;
        final float[] multiply = new float[4];
        final float[] screen = new float[4];

        int maskCount;
        int[] masks = new int[8];

        int vertexCount;
        float[] vertices = new float[128];

        int indexCount;
        short[] indices = new short[64];

        void copyFrom(Drawable other) {
            id = other.id;
            textureId = other.textureId;
            renderOrder = other.renderOrder;
            flags = other.flags;
            opacity = other.opacity;
            System.arraycopy(other.multiply, 0, multiply, 0, 4);
            System.arraycopy(other.screen, 0, screen, 0, 4);

            maskCount = other.maskCount;
            if (masks.length < maskCount) {
                masks = new int[maskCount];
            }
            System.arraycopy(other.masks, 0, masks, 0, maskCount);

            vertexCount = other.vertexCount;
            int floats = vertexCount * 4;
            if (vertices.length < floats) {
                vertices = new float[floats];
            }
            System.arraycopy(other.vertices, 0, vertices, 0, floats);

            indexCount = other.indexCount;
            if (indices.length < indexCount) {
                indices = new short[indexCount];
            }
            System.arraycopy(other.indices, 0, indices, 0, indexCount);
        }
    }

    void read(Protocol.Reader reader) throws IOException {
        reader.u32();
        generation = reader.u64();
        sequence = reader.u64();
        opacity = reader.f32();
        mirror = reader.u8() != 0;
        minX = reader.f32();
        maxX = reader.f32();
        minY = reader.f32();
        maxY = reader.f32();

        int keys = reader.bounded(MAX_KEYS);
        if (keyAssetIds.length < keys) {
            keyAssetIds = new int[keys];
            keySides = new int[keys];
        }
        for (int i = 0; i < keys; i++) {
            keyAssetIds[i] = reader.u32();
            keySides[i] = reader.u8();
        }
        keyCount = keys;

        int count = reader.bounded(MAX_DRAWABLES);
        if (drawables.length < count) {
            Drawable[] grown = new Drawable[count];
            System.arraycopy(drawables, 0, grown, 0, drawables.length);
            drawables = grown;
        }
        for (int i = 0; i < count; i++) {
            Drawable drawable = drawables[i];
            if (drawable == null) {
                drawable = new Drawable();
                drawables[i] = drawable;
            }
            readDrawable(reader, drawable);
        }
        drawableCount = count;
    }

    private static void readDrawable(Protocol.Reader reader, Drawable drawable) throws IOException {
        drawable.id = reader.u32();
        drawable.textureId = reader.u32();
        drawable.renderOrder = reader.i32();
        drawable.flags = reader.u8();
        drawable.opacity = reader.f32();
        for (int i = 0; i < 4; i++) {
            drawable.multiply[i] = reader.f32();
        }
        for (int i = 0; i < 4; i++) {
            drawable.screen[i] = reader.f32();
        }

        int masks = reader.bounded16(MAX_MASKS);
        if (drawable.masks.length < masks) {
            drawable.masks = new int[masks];
        }
        for (int i = 0; i < masks; i++) {
            drawable.masks[i] = reader.u32();
        }
        drawable.maskCount = masks;

        int vertices = reader.bounded16(MAX_VERTICES);
        int floats = vertices * 4;
        if (drawable.vertices.length < floats) {
            drawable.vertices = new float[floats];
        }
        for (int i = 0; i < floats; i++) {
            drawable.vertices[i] = reader.f32();
        }
        drawable.vertexCount = vertices;

        int indices = reader.bounded16(MAX_INDICES);
        if (drawable.indices.length < indices) {
            drawable.indices = new short[indices];
        }
        for (int i = 0; i < indices; i++) {
            drawable.indices[i] = (short) reader.u16();
        }
        drawable.indexCount = indices;
    }

    void copyFrom(Frame other) {        generation = other.generation;
        sequence = other.sequence;
        opacity = other.opacity;
        mirror = other.mirror;
        minX = other.minX;
        maxX = other.maxX;
        minY = other.minY;
        maxY = other.maxY;

        int keys = other.keyCount;
        if (keyAssetIds.length < keys) {
            keyAssetIds = new int[keys];
            keySides = new int[keys];
        }
        System.arraycopy(other.keyAssetIds, 0, keyAssetIds, 0, keys);
        System.arraycopy(other.keySides, 0, keySides, 0, keys);
        keyCount = keys;

        int count = other.drawableCount;
        if (drawables.length < count) {
            Drawable[] grown = new Drawable[count];
            System.arraycopy(drawables, 0, grown, 0, drawables.length);
            drawables = grown;
        }
        for (int i = 0; i < count; i++) {
            Drawable target = drawables[i];
            if (target == null) {
                target = new Drawable();
                drawables[i] = target;
            }
            target.copyFrom(other.drawables[i]);
        }
        drawableCount = count;
    }

    Drawable find(int id) {
        for (int i = 0; i < drawableCount; i++) {
            if (drawables[i].id == id) {
                return drawables[i];
            }
        }
        return null;
    }

    /**
     * Adds one key to this frame's pressed set.
     *
     * <p>Only used by the `fake_key` diagnostic: it exercises the key imagery on
     * a device with no keyboard attached, which is how the overlay's quad and
     * texture path is checked without a publisher on the other end.
     */
    void appendKey(int assetId) {
        if (keyAssetIds.length <= keyCount) {
            keyAssetIds = Arrays.copyOf(keyAssetIds, keyCount + 8);
            keySides = Arrays.copyOf(keySides, keyCount + 8);
        }
        keyAssetIds[keyCount] = assetId;
        keySides[keyCount] = Protocol.SIDE_LEFT;
        keyCount++;
    }
}
