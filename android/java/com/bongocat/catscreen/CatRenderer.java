package com.bongocat.catscreen;

import android.graphics.Bitmap;
import android.opengl.GLES20;
import android.opengl.GLSurfaceView;
import android.opengl.GLUtils;
import android.util.Log;
import android.util.LongSparseArray;
import android.util.SparseIntArray;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.FloatBuffer;
import java.nio.ShortBuffer;
import java.util.Arrays;

import javax.microedition.khronos.egl.EGLConfig;
import javax.microedition.khronos.opengles.GL10;

/**
 * Draws the publisher's evaluated frames.
 *
 * <p>This is the same rasteriser the desktop overlay runs, reduced to GLES 2.0:
 * the publisher hands over the geometry Cubism already evaluated, so every
 * decision left here is the one the D3D11 and Metal backends make — the
 * clip-space transform from the model bounds, premultiplied normal blending,
 * additive and multiplicative modes, back-face culling reversed while the model
 * is mirrored, and a mask target per masked drawable accumulated from the mask
 * source meshes' alpha.
 *
 * <p>Nothing here reads configuration or decides what the cat does. A frame is
 * either drawn as it arrived or dropped.
 */
public final class CatRenderer implements GLSurfaceView.Renderer {

    public interface Stats {
        void onStats(String text, boolean live);
    }

    private static final String TAG = "CatRenderer";

    private static final int VERTEX_STRIDE_BYTES = 16;
    private static final int UV_OFFSET_FLOATS = 2;
    private static final float MODEL_OPACITY_EPSILON = 0.0f;

    /**
     * Mask targets are rasterised at half the panel resolution.
     *
     * <p>A mask is coverage, not colour: it multiplies a drawable's opacity, and
     * half resolution with linear filtering is indistinguishable from full
     * resolution once the result is composited. The mask sources span most of the
     * screen, so at full resolution five targets cost this device a third of its
     * frame rate in fill and framebuffer traffic alone.
     */
    private static final float MASK_SCALE = 0.5f;

    /**
     * Upper bound on the longer side of a mask target, in texels.
     *
     * <p>The mask sources are whole body parts, so the union of a masked drawable
     * and its sources covers most of the panel. Capping the target keeps a large
     * clip cheap while a small one (an eye, a mouth) still gets half resolution.
     */
    private static final int MAX_MASK_TEXELS = 256;

    private static final String VERTEX_SHADER =
            "attribute vec2 aPos;\n"
            + "attribute vec2 aUv;\n"
            + "uniform vec4 uScaleOffset;\n"
            + "varying vec2 vUv;\n"
            + "void main() {\n"
            + "    vUv = vec2(aUv.x, 1.0 - aUv.y);\n"
            + "    gl_Position = vec4(aPos * uScaleOffset.xy + uScaleOffset.zw, 0.0, 1.0);\n"
            + "}\n";

    private static final String MODEL_FRAGMENT =
            "precision mediump float;\n"
            + "varying vec2 vUv;\n"
            + "uniform sampler2D uModelTex;\n"
            + "uniform sampler2D uMaskTex;\n"
            + "uniform vec4 uMultiply;\n"
            + "uniform vec4 uScreen;\n"
            + "uniform vec2 uMaskSize;\n"
            + "uniform vec2 uMaskOrigin;\n"
            + "uniform float uMaskScale;\n"
            + "uniform float uMaskEnabled;\n"
            + "uniform float uMaskInverted;\n"
            + "uniform float uOpacity;\n"
            + "void main() {\n"
            + "    vec4 texel = texture2D(uModelTex, vUv);\n"
            + "    vec3 color = texel.rgb * uMultiply.rgb;\n"
            + "    color = (color + uScreen.rgb * texel.a) - (color * uScreen.rgb);\n"
            + "    float mask = 1.0;\n"
            + "    if (uMaskEnabled > 0.5) {\n"
            + "        mask = texture2D(uMaskTex, (gl_FragCoord.xy * uMaskScale - uMaskOrigin) / uMaskSize).a;\n"
            + "        if (uMaskInverted > 0.5) {\n"
            + "            mask = 1.0 - mask;\n"
            + "        }\n"
            + "    }\n"
            + "    float coverage = uOpacity * mask;\n"
            + "    gl_FragColor = vec4(color * coverage, texel.a * coverage);\n"
            + "}\n";

    private static final String MASK_FRAGMENT =
            "precision mediump float;\n"
            + "varying vec2 vUv;\n"
            + "uniform sampler2D uModelTex;\n"
            + "void main() {\n"
            + "    gl_FragColor = vec4(0.0, 0.0, 0.0, texture2D(uModelTex, vUv).a);\n"
            + "}\n";

    private static final short[] QUAD_INDICES = {0, 1, 2, 0, 2, 3};

    private final PublisherClient client;

    private Stats stats;

    private int modelProgram;
    private int maskProgram;

    private int modelPos;
    private int modelUv;
    private int modelScaleOffset;
    private int modelTex;
    private int modelMaskTex;
    private int modelMultiply;
    private int modelScreen;
    private int modelMaskSize;
    private int modelMaskOrigin;
    private int modelMaskScale;
    private int modelMaskEnabled;
    private int modelMaskInverted;
    private int modelOpacity;

    private int maskPos;
    private int maskUv;
    private int maskScaleOffset;
    private int maskTex;

    private final SparseIntArray textures = new SparseIntArray();
    private final SparseIntArray keyTextures = new SparseIntArray();
    private final LongSparseArray<MaskTarget> maskTargets = new LongSparseArray<>();

    private long[] groupKeys = new long[8];
    private int[][] groupSources = new int[8][];
    private int[][] groupMembers = new int[8][];
    private float[][] groupBounds = new float[8][];
    private long[] groupHashes = new long[8];
    private int groupCount;
    private int[] maskKeyScratch = new int[8];

    private int blankTexture;

    private Scene scene;
    private boolean haveFrame;
    /**
     * The publisher hid the model. Distinct from "no frame yet": a hidden model
     * is a deliberate black panel, not a missing connection, so the status line
     * must stay away.
     */
    private boolean cleared;
    private final Frame frame = new Frame();
    private int[] order = new int[64];

    private ByteBuffer vertexByteBuffer;
    private FloatBuffer vertexPositions;
    private FloatBuffer vertexUvs;
    private ShortBuffer indexScratch;

    private final float[] quadVertices = new float[16];
    private final float[] scaleOffset = new float[4];
    private int skippedMasks;
    private int keysDrawn;
    private int keyMisses;

    private int viewWidth = 1;
    private int viewHeight = 1;

    private int frames;
    private long windowStart;

    private boolean skipMasks;
    private boolean skipModel;
    private int forcedKeyId = -1;

    public CatRenderer(PublisherClient client) {
        this.client = client;
    }

    void setStats(Stats stats) {
        this.stats = stats;
    }

    /** Diagnostic switches, used from `am start --ez` while tuning the device. */
    void setDebugSwitches(boolean skipMasks, boolean skipModel) {
        this.skipMasks = skipMasks;
        this.skipModel = skipModel;
    }

    /** Adds a key to every incoming frame; `-1` disables it. See `fake_key`. */
    void setForcedKey(int assetId) {
        this.forcedKeyId = assetId;
    }

    @Override
    public void onSurfaceCreated(GL10 unused, EGLConfig config) {
        GLES20.glClearColor(0f, 0f, 0f, 1f);
        GLES20.glDisable(GLES20.GL_DEPTH_TEST);
        GLES20.glDisable(GLES20.GL_CULL_FACE);
        GLES20.glDisable(GLES20.GL_SCISSOR_TEST);

        modelProgram = buildProgram(VERTEX_SHADER, MODEL_FRAGMENT);
        modelPos = GLES20.glGetAttribLocation(modelProgram, "aPos");
        modelUv = GLES20.glGetAttribLocation(modelProgram, "aUv");
        modelScaleOffset = GLES20.glGetUniformLocation(modelProgram, "uScaleOffset");
        modelTex = GLES20.glGetUniformLocation(modelProgram, "uModelTex");
        modelMaskTex = GLES20.glGetUniformLocation(modelProgram, "uMaskTex");
        modelMultiply = GLES20.glGetUniformLocation(modelProgram, "uMultiply");
        modelScreen = GLES20.glGetUniformLocation(modelProgram, "uScreen");
        modelMaskSize = GLES20.glGetUniformLocation(modelProgram, "uMaskSize");
        modelMaskOrigin = GLES20.glGetUniformLocation(modelProgram, "uMaskOrigin");
        modelMaskScale = GLES20.glGetUniformLocation(modelProgram, "uMaskScale");
        modelMaskEnabled = GLES20.glGetUniformLocation(modelProgram, "uMaskEnabled");
        modelMaskInverted = GLES20.glGetUniformLocation(modelProgram, "uMaskInverted");
        modelOpacity = GLES20.glGetUniformLocation(modelProgram, "uOpacity");

        maskProgram = buildProgram(VERTEX_SHADER, MASK_FRAGMENT);
        maskPos = GLES20.glGetAttribLocation(maskProgram, "aPos");
        maskUv = GLES20.glGetAttribLocation(maskProgram, "aUv");
        maskScaleOffset = GLES20.glGetUniformLocation(maskProgram, "uScaleOffset");
        maskTex = GLES20.glGetUniformLocation(maskProgram, "uModelTex");

        blankTexture = createBlankTexture();

        Log.i(TAG, "GL_VERSION=" + GLES20.glGetString(GLES20.GL_VERSION));
        Log.i(TAG, "GL_RENDERER=" + GLES20.glGetString(GLES20.GL_RENDERER));
        int[] maxTexture = new int[1];
        GLES20.glGetIntegerv(GLES20.GL_MAX_TEXTURE_SIZE, maxTexture, 0);
        Log.i(TAG, "GL_MAX_TEXTURE_SIZE=" + maxTexture[0]);

        windowStart = System.nanoTime();
        frames = 0;
    }

    @Override
    public void onSurfaceChanged(GL10 unused, int width, int height) {
        viewWidth = Math.max(1, width);
        viewHeight = Math.max(1, height);
        GLES20.glViewport(0, 0, viewWidth, viewHeight);
        // Mask targets are the size of the surface, so a resize invalidates them.
        releaseMaskTargets();
        Log.i(TAG, "SURFACE=" + width + "x" + height);
    }

    @Override
    public void onDrawFrame(GL10 unused) {
        Scene incoming = client.takeScene();
        if (incoming != null) {
            applyScene(incoming);
        }
        if (client.takeCleared()) {
            haveFrame = false;
            cleared = true;
        }
        if (client.copyFrameInto(frame)) {
            haveFrame = true;
            cleared = false;
            if (forcedKeyId >= 0) {
                frame.appendKey(forcedKeyId);
            }
        }

        GLES20.glViewport(0, 0, viewWidth, viewHeight);
        GLES20.glClearColor(0f, 0f, 0f, 1f);
        GLES20.glClear(GLES20.GL_COLOR_BUFFER_BIT);

        if (haveFrame && scene != null) {
            modelTransform();
            if (!skipMasks) {
                renderMasks();
            }
            if (!skipModel) {
                renderModel();
            }
        }
        reportStats();
    }

    // ---------------------------------------------------------------- resources

    private void applyScene(Scene next) {
        releaseSceneResources();
        scene = next;

        for (int i = 0; i < next.textureIds.length; i++) {
            Bitmap image = next.textureImages[i];
            if (image == null) {
                continue;
            }
            textures.put(next.textureIds[i], upload(image));
            image.recycle();
            next.textureImages[i] = null;
        }
        for (int i = 0; i < next.keyIds.length; i++) {
            Bitmap image = next.keyImages[i];
            if (image == null) {
                continue;
            }
            keyTextures.put(next.keyIds[i], upload(image));
            image.recycle();
            next.keyImages[i] = null;
        }
        if (next.background != null) {
            // The background has no id of its own; key -1 keeps it apart from the
            // model's textures.
            textures.put(-1, upload(next.background));
            next.background.recycle();
            next.background = null;
        }
        Log.i(TAG, "scene generation " + next.generation + ": " + textures.size()
                + " textures, " + keyTextures.size() + " key images");
    }

    private void releaseSceneResources() {
        for (int i = 0; i < textures.size(); i++) {
            deleteTexture(textures.valueAt(i));
        }
        textures.clear();
        for (int i = 0; i < keyTextures.size(); i++) {
            deleteTexture(keyTextures.valueAt(i));
        }
        keyTextures.clear();
        releaseMaskTargets();

        if (scene != null) {
            scene.recycle();
            scene = null;
        }
        haveFrame = false;
    }

    private void releaseMaskTargets() {
        for (int i = 0; i < maskTargets.size(); i++) {
            MaskTarget target = maskTargets.valueAt(i);
            GLES20.glDeleteFramebuffers(1, new int[]{target.framebuffer}, 0);
            deleteTexture(target.texture);
        }
        maskTargets.clear();
    }

    private static void deleteTexture(int texture) {
        if (texture != 0) {
            GLES20.glDeleteTextures(1, new int[]{texture}, 0);
        }
    }

    private static int upload(Bitmap image) {
        int[] name = new int[1];
        GLES20.glGenTextures(1, name, 0);
        GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, name[0]);
        GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_MIN_FILTER, GLES20.GL_LINEAR);
        GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_MAG_FILTER, GLES20.GL_LINEAR);
        GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_WRAP_S, GLES20.GL_CLAMP_TO_EDGE);
        GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_WRAP_T, GLES20.GL_CLAMP_TO_EDGE);
        GLUtils.texImage2D(GLES20.GL_TEXTURE_2D, 0, image, 0);
        return name[0];
    }

    private static int createBlankTexture() {
        int[] name = new int[1];
        GLES20.glGenTextures(1, name, 0);
        GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, name[0]);
        GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_MIN_FILTER, GLES20.GL_NEAREST);
        GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_MAG_FILTER, GLES20.GL_NEAREST);
        GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_WRAP_S, GLES20.GL_CLAMP_TO_EDGE);
        GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_WRAP_T, GLES20.GL_CLAMP_TO_EDGE);
        ByteBuffer pixel = ByteBuffer.allocateDirect(4).order(ByteOrder.nativeOrder());
        pixel.put(new byte[]{0, 0, 0, 0}).position(0);
        GLES20.glTexImage2D(GLES20.GL_TEXTURE_2D, 0, GLES20.GL_RGBA, 1, 1, 0,
                GLES20.GL_RGBA, GLES20.GL_UNSIGNED_BYTE, pixel);
        return name[0];
    }

    /**
     * A mask target sized to the pixels its group can actually reach.
     *
     * <p>A full-screen target per masked drawable cost this device a third of its
     * frame rate: several 800x800 colour buffers written and read back every
     * frame is far more bandwidth than the panel can afford. The target now
     * covers only the union of the group's drawables and their mask sources, and
     * the mask pass renders with an offset, scaled viewport so the geometry keeps
     * its screen-space pixel coordinates.
     */
    private MaskTarget maskTargetFor(long key, int width, int height) {
        MaskTarget target = maskTargets.get(key);
        if (target != null && target.width >= width && target.height >= height) {
            return target;
        }
        if (target != null) {
            GLES20.glDeleteFramebuffers(1, new int[]{target.framebuffer}, 0);
            deleteTexture(target.texture);
            maskTargets.remove(key);
        }

        int targetWidth = Math.min(viewWidth, Math.max(width, 64));
        int targetHeight = Math.min(viewHeight, Math.max(height, 64));
        if (target != null) {
            targetWidth = Math.min(viewWidth, Math.max(targetWidth, target.width));
            targetHeight = Math.min(viewHeight, Math.max(targetHeight, target.height));
        }

        int[] texture = new int[1];
        GLES20.glGenTextures(1, texture, 0);
        GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, texture[0]);
        GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_MIN_FILTER, GLES20.GL_LINEAR);
        GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_MAG_FILTER, GLES20.GL_LINEAR);
        GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_WRAP_S, GLES20.GL_CLAMP_TO_EDGE);
        GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_WRAP_T, GLES20.GL_CLAMP_TO_EDGE);
        GLES20.glTexImage2D(GLES20.GL_TEXTURE_2D, 0, GLES20.GL_RGBA, targetWidth, targetHeight, 0,
                GLES20.GL_RGBA, GLES20.GL_UNSIGNED_BYTE, null);

        int[] framebuffer = new int[1];
        GLES20.glGenFramebuffers(1, framebuffer, 0);
        GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, framebuffer[0]);
        GLES20.glFramebufferTexture2D(GLES20.GL_FRAMEBUFFER, GLES20.GL_COLOR_ATTACHMENT0,
                GLES20.GL_TEXTURE_2D, texture[0], 0);
        int status = GLES20.glCheckFramebufferStatus(GLES20.GL_FRAMEBUFFER);
        GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, 0);
        if (status != GLES20.GL_FRAMEBUFFER_COMPLETE) {
            GLES20.glDeleteFramebuffers(1, framebuffer, 0);
            deleteTexture(texture[0]);
            throw new IllegalStateException("mask framebuffer incomplete: 0x"
                    + Integer.toHexString(status));
        }

        target = new MaskTarget(texture[0], framebuffer[0], targetWidth, targetHeight);
        maskTargets.put(key, target);
        return target;
    }

    // ------------------------------------------------------------------ drawing

    /** The clip-space transform the D3D11 and Metal backends use, verbatim. */
    private void modelTransform() {
        float modelWidth = Math.max(frame.maxX - frame.minX, 1e-6f);
        float modelHeight = Math.max(frame.maxY - frame.minY, 1e-6f);
        float centerX = (frame.minX + frame.maxX) * 0.5f;
        float centerY = (frame.minY + frame.maxY) * 0.5f;
        float modelAspect = modelWidth / modelHeight;
        float viewportAspect = (float) viewWidth / (float) viewHeight;

        float scaleX = 2f / modelWidth;
        float scaleY = 2f / modelHeight;
        if (viewportAspect > modelAspect) {
            scaleX *= modelAspect / viewportAspect;
        } else {
            scaleY *= viewportAspect / modelAspect;
        }

        float offsetX = -centerX * scaleX;
        if (frame.mirror) {
            scaleX = -scaleX;
            offsetX = -offsetX;
        }
        scaleOffset[0] = scaleX;
        scaleOffset[1] = scaleY;
        scaleOffset[2] = offsetX;
        scaleOffset[3] = -centerY * scaleY;
    }

    /**
     * Builds one mask target per distinct set of mask sources.
     *
     * <p>A Cubism model masks several drawables with the same mesh: an eye, a
     * brow and a mouth are all clipped by one head outline. Rendering a target
     * per masked drawable repeated the same framebuffer pass two or three times
     * per frame, and on this device each pass costs more than the panel's whole
     * frame budget. Grouping them by source set cuts the passes to the number of
     * distinct clips, which for the bundled model is three instead of five.
     */
    private void renderMasks() {
        collectMaskGroups();
        if (groupCount == 0) {
            return;
        }

        GLES20.glUseProgram(maskProgram);
        GLES20.glUniform4fv(maskScaleOffset, 1, scaleOffset, 0);
        GLES20.glEnable(GLES20.GL_BLEND);
        blendNormal();
        GLES20.glDisable(GLES20.GL_CULL_FACE);
        GLES20.glUniform1i(maskTex, 0);
        GLES20.glActiveTexture(GLES20.GL_TEXTURE0);

        for (int g = 0; g < groupCount; g++) {
            float[] bounds = groupBounds[g];
            int requiredWidth = Math.min(viewWidth,
                    (int) Math.ceil(bounds[2]) - (int) Math.floor(bounds[0]) + 2);
            int requiredHeight = Math.min(viewHeight,
                    (int) Math.ceil(bounds[3]) - (int) Math.floor(bounds[1]) + 2);
            if (requiredWidth <= 0 || requiredHeight <= 0) {
                continue;
            }

            float scale = Math.min(MASK_SCALE,
                    (float) MAX_MASK_TEXELS / Math.max(requiredWidth, requiredHeight));
            int texelsWide = Math.min(viewWidth,
                    Math.max(1, (int) Math.ceil(requiredWidth * scale)));
            int texelsHigh = Math.min(viewHeight,
                    Math.max(1, (int) Math.ceil(requiredHeight * scale)));
            MaskTarget target = maskTargetFor(groupKeys[g], texelsWide, texelsHigh);

            // A clip only changes when the geometry it is cut from moves. On a
            // still cat that is never, which is what lets the panel run at its
            // own refresh rate instead of the rasteriser's.
            if (target.valid && target.sourceHash == groupHashes[g] && target.scale == scale) {
                skippedMasks++;
                continue;
            }

            target.scale = scale;
            target.originX = Math.max(0, Math.min(target.width - 1,
                    (int) Math.floor(bounds[0] * scale) - 1));
            target.originY = Math.max(0, Math.min(target.height - 1,
                    (int) Math.floor(bounds[1] * scale) - 1));

            GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, target.framebuffer);
            // The offset, scaled viewport keeps the source meshes in the same
            // screen-space pixel coordinates the sampler inverts.
            GLES20.glViewport(-target.originX, -target.originY,
                    Math.max(1, Math.round(viewWidth * scale)),
                    Math.max(1, Math.round(viewHeight * scale)));
            GLES20.glClearColor(0f, 0f, 0f, 0f);
            GLES20.glClear(GLES20.GL_COLOR_BUFFER_BIT);

            int[] sources = groupSources[g];
            for (int s = 0; s < sources.length; s++) {
                Frame.Drawable source = frame.find(sources[s]);
                if (source == null || source.indexCount == 0 || source.vertexCount == 0) {
                    continue;
                }
                int texture = textures.get(source.textureId, 0);
                if (texture == 0) {
                    continue;
                }
                GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, texture);
                drawMesh(maskPos, maskUv, source);
            }
            target.sourceHash = groupHashes[g];
            target.valid = true;
        }

        GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, 0);
        GLES20.glViewport(0, 0, viewWidth, viewHeight);
    }

    /**
     * Buckets the frame's masked drawables by their mask source set, then unions
     * the pixel-space extent and geometry hash of every member and source.
     *
     * <p>The extent has to cover the members, not just the sources: the target is
     * sampled at the masked drawable's own fragments, so a pixel of the drawable
     * outside the target would read a clamped texel. Sources are included as well
     * so linear filtering at the edge has data to interpolate with.
     */
    private void collectMaskGroups() {
        groupCount = 0;

        for (int i = 0; i < frame.drawableCount; i++) {
            Frame.Drawable masked = frame.drawables[i];
            if (masked.maskCount == 0
                    || (masked.flags & Protocol.FLAG_VISIBLE) == 0
                    || masked.opacity <= MODEL_OPACITY_EPSILON
                    || masked.vertexCount == 0) {
                continue;
            }
            long key = maskKey(masked);
            int group = -1;
            for (int g = 0; g < groupCount; g++) {
                if (groupKeys[g] == key) {
                    group = g;
                    break;
                }
            }
            if (group < 0) {
                group = addGroup(key, masked);
            } else {
                groupMembers[group] = append(groupMembers[group], masked.id);
            }
        }

        for (int g = 0; g < groupCount; g++) {
            float[] bounds = groupBounds[g];
            bounds[0] = Float.MAX_VALUE;
            bounds[1] = Float.MAX_VALUE;
            bounds[2] = -Float.MAX_VALUE;
            bounds[3] = -Float.MAX_VALUE;

            long hash = 1469598103934665603L;
            hash = mix(hash, Float.floatToIntBits(scaleOffset[0]));
            hash = mix(hash, Float.floatToIntBits(scaleOffset[1]));
            hash = mix(hash, Float.floatToIntBits(scaleOffset[2]));
            hash = mix(hash, Float.floatToIntBits(scaleOffset[3]));

            int[] members = groupMembers[g];
            for (int m = 0; m < members.length; m++) {
                hash = accumulate(bounds, hash, frame.find(members[m]));
            }
            int[] sources = groupSources[g];
            for (int s = 0; s < sources.length; s++) {
                hash = accumulate(bounds, hash, frame.find(sources[s]));
            }
            groupHashes[g] = hash;

            if (bounds[0] > bounds[2] || bounds[1] > bounds[3]) {
                bounds[2] = bounds[0];
                bounds[3] = bounds[1];
            }
        }
    }

    private int addGroup(long key, Frame.Drawable masked) {
        if (groupCount == groupKeys.length) {
            int grown = groupCount * 2;
            groupKeys = Arrays.copyOf(groupKeys, grown);
            groupSources = Arrays.copyOf(groupSources, grown);
            groupMembers = Arrays.copyOf(groupMembers, grown);
            groupBounds = Arrays.copyOf(groupBounds, grown);
            groupHashes = Arrays.copyOf(groupHashes, grown);
        }

        int count = masked.maskCount;
        int[] sources = new int[count];
        System.arraycopy(masked.masks, 0, sources, 0, count);
        Arrays.sort(sources);

        groupKeys[groupCount] = key;
        groupSources[groupCount] = sources;
        groupMembers[groupCount] = new int[]{masked.id};
        groupBounds[groupCount] = new float[4];
        groupCount++;
        return groupCount - 1;
    }

    private static int[] append(int[] values, int value) {
        int[] grown = Arrays.copyOf(values, values.length + 1);
        grown[values.length] = value;
        return grown;
    }

    /** Adds one mesh's vertices to {@code bounds} and to the geometry hash. */
    private long accumulate(float[] bounds, long hash, Frame.Drawable drawable) {
        if (drawable == null) {
            return hash;
        }
        int count = drawable.vertexCount;
        hash = mix(hash, count);
        for (int v = 0; v < count; v++) {
            int at = v * 4;
            float x = drawable.vertices[at];
            float y = drawable.vertices[at + 1];
            hash = mix(hash, Float.floatToIntBits(x));
            hash = mix(hash, Float.floatToIntBits(y));

            float clipX = x * scaleOffset[0] + scaleOffset[2];
            float clipY = y * scaleOffset[1] + scaleOffset[3];
            float pixelX = (clipX * 0.5f + 0.5f) * viewWidth;
            float pixelY = (clipY * 0.5f + 0.5f) * viewHeight;
            if (pixelX < bounds[0]) {
                bounds[0] = pixelX;
            }
            if (pixelY < bounds[1]) {
                bounds[1] = pixelY;
            }
            if (pixelX > bounds[2]) {
                bounds[2] = pixelX;
            }
            if (pixelY > bounds[3]) {
                bounds[3] = pixelY;
            }
        }
        return hash;
    }

    /** A stable key for the set of meshes masking one drawable. */
    private long maskKey(Frame.Drawable drawable) {
        int count = drawable.maskCount;
        if (maskKeyScratch.length < count) {
            maskKeyScratch = new int[count];
        }
        System.arraycopy(drawable.masks, 0, maskKeyScratch, 0, count);
        Arrays.sort(maskKeyScratch, 0, count);

        long hash = 1469598103934665603L;
        hash = mix(hash, count);
        for (int m = 0; m < count; m++) {
            hash = mix(hash, maskKeyScratch[m]);
        }
        return hash;
    }

    private static long mix(long hash, int value) {
        return (hash ^ value) * 1099511628211L;
    }

    private void renderModel() {
        GLES20.glUseProgram(modelProgram);
        GLES20.glUniform4fv(modelScaleOffset, 1, scaleOffset, 0);
        GLES20.glUniform1i(modelTex, 0);
        GLES20.glUniform1i(modelMaskTex, 1);
        GLES20.glEnable(GLES20.GL_BLEND);
        GLES20.glActiveTexture(GLES20.GL_TEXTURE0);

        if (textures.indexOfKey(-1) >= 0) {
            int texture = textures.get(-1, 0);
            if (texture != 0) {
                GLES20.glDisable(GLES20.GL_CULL_FACE);
                blendNormal();
                GLES20.glUniform4f(modelMultiply, 1f, 1f, 1f, 1f);
                GLES20.glUniform4f(modelScreen, 0f, 0f, 0f, 0f);
                GLES20.glUniform1f(modelMaskEnabled, 0f);
                GLES20.glUniform1f(modelMaskInverted, 0f);
                GLES20.glUniform1f(modelOpacity, 1f);
                GLES20.glActiveTexture(GLES20.GL_TEXTURE1);
                GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, blankTexture);
                GLES20.glActiveTexture(GLES20.GL_TEXTURE0);
                GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, texture);
                drawQuad(modelPos, modelUv,
                        scene.backgroundMinX, scene.backgroundMaxX,
                        scene.backgroundMinY, scene.backgroundMaxY);
            }
        }

        sortDrawables();
        for (int i = 0; i < frame.drawableCount; i++) {
            Frame.Drawable drawable = frame.drawables[order[i]];
            if ((drawable.flags & Protocol.FLAG_VISIBLE) == 0
                    || drawable.opacity <= MODEL_OPACITY_EPSILON
                    || drawable.indexCount == 0
                    || drawable.vertexCount == 0) {
                continue;
            }
            int texture = textures.get(drawable.textureId, 0);
            if (texture == 0) {
                continue;
            }

            applyBlend(drawable.flags);
            applyCulling(drawable.flags);

            GLES20.glUniform4fv(modelMultiply, 1, drawable.multiply, 0);
            GLES20.glUniform4fv(modelScreen, 1, drawable.screen, 0);
            GLES20.glUniform1f(modelOpacity, drawable.opacity * frame.opacity);

            int maskTexture = blankTexture;
            if (drawable.maskCount > 0) {
                MaskTarget target = maskTargets.get(maskKey(drawable));
                if (target == null) {
                    continue;
                }
                maskTexture = target.texture;
                GLES20.glUniform1f(modelMaskEnabled, 1f);
                GLES20.glUniform1f(modelMaskInverted,
                        (drawable.flags & Protocol.FLAG_INVERTED_MASK) != 0 ? 1f : 0f);
                GLES20.glUniform2f(modelMaskOrigin, target.originX, target.originY);
                GLES20.glUniform2f(modelMaskSize, target.width, target.height);
                GLES20.glUniform1f(modelMaskScale, target.scale);
            } else {
                GLES20.glUniform1f(modelMaskEnabled, 0f);
                GLES20.glUniform1f(modelMaskInverted, 0f);
            }

            GLES20.glActiveTexture(GLES20.GL_TEXTURE1);
            GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, maskTexture);
            GLES20.glActiveTexture(GLES20.GL_TEXTURE0);
            GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, texture);
            drawMesh(modelPos, modelUv, drawable);
        }

        // Pressed-key imagery is the topmost layer above the model.
        GLES20.glDisable(GLES20.GL_CULL_FACE);
        blendNormal();
        GLES20.glUniform4f(modelMultiply, 1f, 1f, 1f, 1f);
        GLES20.glUniform4f(modelScreen, 0f, 0f, 0f, 0f);
        GLES20.glUniform1f(modelMaskEnabled, 0f);
        GLES20.glUniform1f(modelMaskInverted, 0f);
        GLES20.glUniform1f(modelOpacity, 1f);
        GLES20.glActiveTexture(GLES20.GL_TEXTURE1);
        GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, blankTexture);
        GLES20.glActiveTexture(GLES20.GL_TEXTURE0);

        for (int i = 0; i < frame.keyCount; i++) {
            int texture = keyTextures.get(frame.keyAssetIds[i], 0);
            if (texture == 0) {
                keyMisses++;
                continue;
            }
            int base = findKeyBounds(frame.keyAssetIds[i]);
            if (base < 0) {
                keyMisses++;
                continue;
            }
            GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, texture);
            drawQuad(modelPos, modelUv,
                    scene.keyBounds[base],
                    scene.keyBounds[base + 1],
                    scene.keyBounds[base + 2],
                    scene.keyBounds[base + 3]);
            keysDrawn++;
        }
    }

    private int findKeyBounds(int keyId) {
        for (int i = 0; i < scene.keyIds.length; i++) {
            if (scene.keyIds[i] == keyId) {
                return i * 4;
            }
        }
        return -1;
    }

    /** Draw order is `(render_order, id)`, matching both native backends. */
    private void sortDrawables() {
        int count = frame.drawableCount;
        if (order.length < count) {
            order = new int[count];
        }
        for (int i = 0; i < count; i++) {
            order[i] = i;
        }
        for (int i = 1; i < count; i++) {
            int current = order[i];
            Frame.Drawable value = frame.drawables[current];
            int j = i - 1;
            while (j >= 0 && isAfter(frame.drawables[order[j]], value)) {
                order[j + 1] = order[j];
                j--;
            }
            order[j + 1] = current;
        }
    }

    private static boolean isAfter(Frame.Drawable left, Frame.Drawable right) {
        if (left.renderOrder != right.renderOrder) {
            return left.renderOrder > right.renderOrder;
        }
        return left.id > right.id;
    }

    private static void blendNormal() {
        GLES20.glBlendFuncSeparate(
                GLES20.GL_ONE, GLES20.GL_ONE_MINUS_SRC_ALPHA,
                GLES20.GL_ONE, GLES20.GL_ONE_MINUS_SRC_ALPHA);
    }

    private static void applyBlend(int flags) {
        int mode = (flags & Protocol.FLAG_BLEND_MASK) >> Protocol.FLAG_BLEND_SHIFT;
        switch (mode) {
            case Protocol.BLEND_ADDITIVE:
                GLES20.glBlendFuncSeparate(
                        GLES20.GL_ONE, GLES20.GL_ONE,
                        GLES20.GL_ZERO, GLES20.GL_ONE);
                break;
            case Protocol.BLEND_MULTIPLICATIVE:
                GLES20.glBlendFuncSeparate(
                        GLES20.GL_DST_COLOR, GLES20.GL_ONE_MINUS_SRC_ALPHA,
                        GLES20.GL_ZERO, GLES20.GL_ONE);
                break;
            default:
                blendNormal();
                break;
        }
    }

    private void applyCulling(int flags) {
        if ((flags & Protocol.FLAG_DOUBLE_SIDED) != 0) {
            GLES20.glDisable(GLES20.GL_CULL_FACE);
            return;
        }
        GLES20.glEnable(GLES20.GL_CULL_FACE);
        // A mirrored model reverses winding, so the opposite face is culled.
        GLES20.glCullFace(frame.mirror ? GLES20.GL_FRONT : GLES20.GL_BACK);
    }

    private void drawMesh(int posLoc, int uvLoc, Frame.Drawable drawable) {
        bindVertices(drawable);
        GLES20.glEnableVertexAttribArray(posLoc);
        GLES20.glVertexAttribPointer(posLoc, 2, GLES20.GL_FLOAT, false,
                VERTEX_STRIDE_BYTES, vertexPositions);
        GLES20.glEnableVertexAttribArray(uvLoc);
        GLES20.glVertexAttribPointer(uvLoc, 2, GLES20.GL_FLOAT, false,
                VERTEX_STRIDE_BYTES, vertexUvs);
        GLES20.glDrawElements(GLES20.GL_TRIANGLES, drawable.indexCount,
                GLES20.GL_UNSIGNED_SHORT, indexScratch);
        GLES20.glDisableVertexAttribArray(posLoc);
        GLES20.glDisableVertexAttribArray(uvLoc);
    }

    private void drawQuad(int posLoc, int uvLoc, float minX, float maxX, float minY, float maxY) {
        quadVertices[0] = minX;
        quadVertices[1] = minY;
        quadVertices[2] = 0f;
        quadVertices[3] = 0f;
        quadVertices[4] = maxX;
        quadVertices[5] = minY;
        quadVertices[6] = 1f;
        quadVertices[7] = 0f;
        quadVertices[8] = maxX;
        quadVertices[9] = maxY;
        quadVertices[10] = 1f;
        quadVertices[11] = 1f;
        quadVertices[12] = minX;
        quadVertices[13] = maxY;
        quadVertices[14] = 0f;
        quadVertices[15] = 1f;

        ensureVertexCapacity(16);
        vertexPositions.clear();
        vertexPositions.put(quadVertices, 0, 16);
        vertexPositions.position(0);
        vertexUvs.position(UV_OFFSET_FLOATS);

        ensureIndexCapacity(QUAD_INDICES.length);
        indexScratch.clear();
        indexScratch.put(QUAD_INDICES);
        indexScratch.position(0);

        GLES20.glEnableVertexAttribArray(posLoc);
        GLES20.glVertexAttribPointer(posLoc, 2, GLES20.GL_FLOAT, false,
                VERTEX_STRIDE_BYTES, vertexPositions);
        GLES20.glEnableVertexAttribArray(uvLoc);
        GLES20.glVertexAttribPointer(uvLoc, 2, GLES20.GL_FLOAT, false,
                VERTEX_STRIDE_BYTES, vertexUvs);
        GLES20.glDrawElements(GLES20.GL_TRIANGLES, QUAD_INDICES.length,
                GLES20.GL_UNSIGNED_SHORT, indexScratch);
        GLES20.glDisableVertexAttribArray(posLoc);
        GLES20.glDisableVertexAttribArray(uvLoc);
    }

    private void bindVertices(Frame.Drawable drawable) {
        int floats = drawable.vertexCount * 4;
        ensureVertexCapacity(floats);
        vertexPositions.clear();
        vertexPositions.put(drawable.vertices, 0, floats);
        vertexPositions.position(0);
        vertexUvs.position(UV_OFFSET_FLOATS);

        ensureIndexCapacity(drawable.indexCount);
        indexScratch.clear();
        indexScratch.put(drawable.indices, 0, drawable.indexCount);
        indexScratch.position(0);
    }

    private void ensureVertexCapacity(int floats) {
        if (vertexPositions != null && vertexPositions.capacity() >= floats) {
            return;
        }
        int capacity = Math.max(floats, 4096);
        vertexByteBuffer = ByteBuffer.allocateDirect(capacity * 4)
                .order(ByteOrder.nativeOrder());
        vertexPositions = vertexByteBuffer.asFloatBuffer();
        vertexUvs = vertexByteBuffer.asFloatBuffer();
    }

    private void ensureIndexCapacity(int indices) {
        if (indexScratch != null && indexScratch.capacity() >= indices) {
            return;
        }
        int capacity = Math.max(indices, 2048);
        indexScratch = ByteBuffer.allocateDirect(capacity * 2)
                .order(ByteOrder.nativeOrder())
                .asShortBuffer();
    }

    // -------------------------------------------------------------------- stats

    private void reportStats() {
        frames++;
        long now = System.nanoTime();
        long elapsed = now - windowStart;
        if (elapsed < 1_000_000_000L) {
            return;
        }
        float fps = frames * 1_000_000_000f / elapsed;
        frames = 0;
        windowStart = now;

        boolean live = cleared || (haveFrame && scene != null);
        String text = String.format("%s  %.1f fps  %.0fx%.0f  masks skipped %d  keys %d/%d",
                live ? "CatScreen" : "waiting", fps, (float) viewWidth, (float) viewHeight,
                skippedMasks, keysDrawn, keyMisses);
        Log.i(TAG, text);
        skippedMasks = 0;
        keysDrawn = 0;
        keyMisses = 0;

        Stats sink = stats;
        if (sink != null) {
            sink.onStats(text, live);
        }
    }

    private static int buildProgram(String vertexSource, String fragmentSource) {
        int vertex = compile(GLES20.GL_VERTEX_SHADER, vertexSource);
        int fragment = compile(GLES20.GL_FRAGMENT_SHADER, fragmentSource);
        int program = GLES20.glCreateProgram();
        GLES20.glAttachShader(program, vertex);
        GLES20.glAttachShader(program, fragment);
        GLES20.glLinkProgram(program);
        int[] status = new int[1];
        GLES20.glGetProgramiv(program, GLES20.GL_LINK_STATUS, status, 0);
        if (status[0] != GLES20.GL_TRUE) {
            throw new IllegalStateException("link failed: "
                    + GLES20.glGetProgramInfoLog(program));
        }
        GLES20.glDeleteShader(vertex);
        GLES20.glDeleteShader(fragment);
        return program;
    }

    private static int compile(int type, String source) {
        int shader = GLES20.glCreateShader(type);
        GLES20.glShaderSource(shader, source);
        GLES20.glCompileShader(shader);
        int[] status = new int[1];
        GLES20.glGetShaderiv(shader, GLES20.GL_COMPILE_STATUS, status, 0);
        if (status[0] != GLES20.GL_TRUE) {
            throw new IllegalStateException("compile failed: "
                    + GLES20.glGetShaderInfoLog(shader));
        }
        return shader;
    }

    private static final class MaskTarget {
        final int texture;
        final int framebuffer;
        final int width;
        final int height;
        int originX;
        int originY;
        float scale = MASK_SCALE;
        boolean valid;
        long sourceHash;

        MaskTarget(int texture, int framebuffer, int width, int height) {
            this.texture = texture;
            this.framebuffer = framebuffer;
            this.width = width;
            this.height = height;
        }
    }
}
