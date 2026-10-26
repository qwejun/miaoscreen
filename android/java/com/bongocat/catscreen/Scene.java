package com.bongocat.catscreen;

import android.graphics.Bitmap;

/**
 * The resources of one model generation: every texture and key image a frame can
 * name, plus the model-space quad each key image is drawn on.
 *
 * <p>Decoded on the network thread, uploaded to GL on the render thread, and
 * dropped as a whole when the publisher announces a new generation.
 */
final class Scene {

    static final int MAX_TEXTURES = 256;
    static final int MAX_KEYS = 512;

    long generation;

    Bitmap background;
    float backgroundMinX;
    float backgroundMaxX;
    float backgroundMinY;
    float backgroundMaxY;

    int[] textureIds = new int[0];
    Bitmap[] textureImages = new Bitmap[0];

    int[] keyIds = new int[0];
    int[] keySides = new int[0];
    float[] keyBounds = new float[0];
    Bitmap[] keyImages = new Bitmap[0];

    /** Drops every decoded bitmap; safe to call more than once. */
    void recycle() {
        if (background != null) {
            background.recycle();
            background = null;
        }
        for (Bitmap image : textureImages) {
            if (image != null) {
                image.recycle();
            }
        }
        for (Bitmap image : keyImages) {
            if (image != null) {
                image.recycle();
            }
        }
        textureImages = new Bitmap[0];
        keyImages = new Bitmap[0];
    }
}
