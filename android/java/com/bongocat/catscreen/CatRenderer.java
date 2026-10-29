package com.bongocat.catscreen;

import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.LinearGradient;
import android.graphics.Paint;
import android.graphics.Path;
import android.graphics.PorterDuff;
import android.graphics.Shader;
import android.graphics.Typeface;
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
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Calendar;
import java.util.HashMap;
import java.util.Map;

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
     * How long a frame stays current once it has been received.
     *
     * <p>The publisher streams at its own pace, so the panel expects several
     * frames per tick of this clock. A second of silence is far outside that and
     * far inside what a person notices, which is what makes it a usable
     * threshold for calling the link dead.
     */
    private static final long STALE_AFTER_MS = 1000;

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

    /**
     * The movement, in panel pixels, the cached-clip check ignores.
     *
     * <p>A clip is only re-rasterised when the geometry it is cut from moved by
     * at least this much. One pixel is deliberately below what the mask's own
     * half-resolution target can express, so a redraw this check skips could not
     * have changed the mask anyway.
     */
    private static final float MASK_HASH_QUANTUM = 1f;

    /**
     * The step, in panel pixels, a mask target's size is rounded up to.
     *
     * <p>The target's scale is derived from its size, and a clip is only
     * re-rasterised when that scale is unchanged, so a size that followed the
     * geometry to the pixel made one pixel of jitter rebuild the clip and
     * re-enter the framebuffer every single frame. Rounding the size up to a
     * fixed step keeps the scale — and therefore the cache — stable while the
     * cat is merely breathing.
     */
    private static final int MASK_TARGET_STEP = 16;

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

    /**
     * The bottom strip is one pre-rendered texture covering the band, so its
     * fragment shader is a plain copy: everything the band shows was rasterised
     * into a bitmap by this class once its text actually changed.
     */
    private static final String OVERLAY_FRAGMENT =
            "precision mediump float;\n"
            + "varying vec2 vUv;\n"
            + "uniform sampler2D uModelTex;\n"
            + "void main() {\n"
            + "    vec4 c = texture2D(uModelTex, vUv);\n"
            // The strip is rasterised by Canvas, which writes straight alpha,
            // and it is blended with premultiplied factors. Multiplying here is
            // what makes a fade actually fade: without it the half-transparent
            // rows of the gradient are added at full strength and the top edge
            // glows white instead of dissolving into the picture.
            + "    gl_FragColor = vec4(c.rgb * c.a, c.a);\n"
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
    /** When the newest frame arrived, for telling a paused cat from a dead link. */
    private long lastFrameNanos;
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

    /**
     * How often the host gets a snapshot while its settings window is open.
     *
     * <p>Each snapshot costs a full-surface read back and a JPEG encode on this
     * thread, so the rate is deliberately low: the preview is for judging layout
     * and content, not for motion.
     */
    private static final long PREVIEW_INTERVAL_NANOS = 2_000_000_000L;
    /** Snapshots travel at the preview's own size; the circle is drawn at 320px. */
    private static final int PREVIEW_SIZE = 320;
    private static final int PREVIEW_QUALITY = 62;

    private long lastPreviewNanos;
    /** True while the host is between asking for a snapshot and getting one. */
    private boolean previewWasRequested;
    private int[] previewPixels;
    private int[] previewRotated;

    // ------------------------------------------------------------------ band
    // The strip under the picture. Its layout rule: the picture keeps its
    // centre while there is room, and is pushed up only once the band would
    // cover its bottom edge.
    //
    // A band taller than most of the surface is not a bar at all — it is a
    // layer over the whole picture, and the host uses that to put the clock in
    // the black above the cat rather than in the sliver at the bottom where a
    // round panel has no width left. Such a layer draws no background and
    // moves nothing: only its text reaches the glass.

    /** Above this share of the surface the band becomes a layer over it. */
    private static final float BAND_LAYER_SHARE = 0.60f;
    /** Clearance between the picture's bottom and the top of the band. */
    private static final int BAND_BOTTOM_MARGIN_PX = 6;
    /** Inset for left- and right-aligned strip text. */
    private static final int BAND_TEXT_MARGIN_PX = 24;
    /** Extra clearance between the round bezel and the outermost glyph. */
    private static final int BAND_TEXT_EDGE_PADDING_PX = 12;
    private static final int BAND_DIVIDER_COLOR = 0x33FFFFFF;
    /**
     * How much of the strip's height is spent dissolving into the picture.
     *
     * <p>The strip is tall enough to start inside the artwork, so drawing it as
     * a solid bar would cut the desk off with a straight line. The top share of
     * it fades from fully transparent to the strip's own background instead,
     * and the clock sits at the end of that fade — the number reads as
     * something the desk is sinking into rather than as a label stuck under the
     * picture.
     */
    private static final float BAND_FADE_SHARE = 0.40f;
    /**
     * Most rows one strip line may be broken into.
     *
     * <p>Three is where the bottom of an 800px panel runs out: the third row
     * already sits 80px below the line's own centre, where the chord is down to
     * 380px, and a fourth would be narrower than the words that need it. A
     * line that still does not fit at three rows is drawn at three and clipped
     * by the bezel, which is the honest failure — better than silently
     * dropping the tail of what someone wrote.
     */
    private static final int MAX_TEXT_ROWS = 3;
    /** Row pitch as a multiple of the text size. */
    private static final float TEXT_LINE_HEIGHT_SHARE = 1.34f;
    /**
     * How full one row has to be before breaking it in two is an improvement.
     *
     * <p>Below this the line already reads as a line, and splitting it would
     * pair two short fragments for no gain — a six-character greeting on two
     * rows looks like a mistake. Measured as a share of the chord rather than
     * as a character count so it holds at every point size.
     */
    private static final float TWO_ROW_FILL_SHARE = 0.62f;

    // ------------------------------------------------------------- clock card
    // A white note pinned inside the upper-left arc, holding the time and the
    // date. The geometry is stated on an 800px panel and scaled to whatever the
    // surface actually is, because it is the panel's own shape that decides
    // where the corner still has room: at (120,158) the card's farthest corner
    // sits 370px from the centre, which leaves 30px of clearance to the bezel.
    //
    // The card is sized to its contents, not to the corner: a note that fills
    // the space it is allowed stops reading as a note and starts competing with
    // the picture. Nothing about it is configurable — a card with a movable
    // corner would need the host to know this panel's curvature, and the one
    // thing the host is not allowed to know is the glass.

    /** Top-left corner of the card on an 800px panel. */
    private static final int CARD_X = 120;
    private static final int CARD_Y = 158;
    private static final int CARD_WIDTH = 148;
    private static final int CARD_HEIGHT = 106;
    private static final float CARD_RADIUS = 14f;
    /**
     * The card's title strip: a lighter band across the top, split from the
     * paper by a hairline. It carries no text — the fold mark and the paw are
     * what make the block read as a card rather than as a white rectangle
     * dropped on the picture.
     */
    private static final int CARD_HEADER_HEIGHT = 30;
    private static final int CARD_FILL = 0xFFFFFFFF;
    private static final int CARD_HEADER_FILL = 0xFFF6F6F8;
    private static final int CARD_HAIRLINE = 0xFFE3E3E6;
    private static final int CARD_FOLD_COLOR = 0xFF9A9AA0;
    private static final int CARD_PAW_COLOR = 0xFFFF8FA8;
    /** The panel the card's geometry above was measured on. */
    private static final float CARD_REFERENCE_PX = 800f;
    /**
     * What a card line is drawn in when the card itself cannot be drawn.
     *
     * <p>Its own colour is a near-black chosen to sit on white; on bare artwork
     * that is invisible. This is the light grey the rest of the layer uses.
     */
    private static final int CARD_TEXT_ON_ARTWORK = 0xFFE8_EAED;

    /** The decorative Chinese face the panel's firmware ships. */
    private static final String ROUNDED_FONT_PATH = "/system/fonts/FZLanTYJW-Regular.otf";
    private static final String ROUNDED_BOLD_FONT_PATH = "/system/fonts/FZLanTYJW-Bold.otf";
    private static final String[] WEEKDAYS =
            {"周日", "周一", "周二", "周三", "周四", "周五", "周六"};

    /**
     * The sentence pools the host's `{random}` marker rotates through.
     *
     * <p>They live here rather than on the host because the rotation has to keep
     * running while the host is closed, and because a clock that only advances
     * when something reconnects is not a clock. The marker may name which pools
     * are in rotation and carry the person's own sentences after it —
     * `{random:heal,moe|自定义一句}` — and the hour, not a counter, decides
     * which line of the merge is showing, so a restart lands on the line the
     * panel was already showing.
     */
    private static final String[] POOL_HEAL = {
            "今天也要加油",
            "慢慢来，比较快",
            "你已经很努力了",
            "先喝口水吧",
            "摸鱼也是生产力",
            "别急，来得及",
            "此刻正好",
            "一起加油喵",
            "深呼吸，再来一次",
            "小事也有意义",
            "今天会很好",
            "猫陪你上班",
            "专注二十分钟",
            "记得站起来走走",
            "比昨天好一点",
            "想休息就休息",
            "有点累也正常",
            "保持呼吸",
            "你值得被夸",
            "晚上早点睡",
    };

    private static final String[] POOL_FUN = {
            "键盘上的猫毛是常态",
            "今日bug已承包给猫",
            "摸鱼进度99%",
            "已进入省电模式，勿扰",
            "工位是我的，猫也是",
            "这条标语没有KPI",
            "错误提示：请先摸猫",
            "系统提示：该休息了",
            "咖啡和猫都要有",
            "假装很忙中",
            "带薪吸猫，合法合规",
            "今日运势：宜摸鱼",
    };

    private static final String[] POOL_POEM = {
            "风把云吹慢了",
            "光是斜的，正好",
            "下午三点，猫打了个哈欠",
            "窗外的天不错",
            "茶凉了，正好歇会",
            "尘埃在光里慢慢落",
            "黄昏提前来了",
            "雨停了，路上有光",
            "夜色刚漫过窗台",
            "今天云像一只猫",
            "风扇把夏天摇慢了",
            "杯子里有一小片天",
    };

    private static final String[] POOL_MOE = {
            "今天也要喵喵向前",
            "猫猫已上线",
            "摸鱼喵咖营业中",
            "困了喵，小憩一下",
            "喵！主人加油",
            "喵呜~摸摸我",
            "猫猫给你打气中",
            "一起喵喵喵",
            "今日份猫猫已投喂",
            "喵~要不要歇会",
            "本喵批准你休息",
            "喵喵打卡成功",
    };

    /** Lines for the hour you are in, five buckets over the day. */
    private static final String[][] TIME_POOLS = {
            { // early morning, 5-10
                    "早，猫比你先醒",
                    "早安，今天从猫开始",
                    "豆浆油条，或者随便一点",
                    "新的一天，猫先上岗",
            },
            { // noon, 11-13
                    "午饭别糊弄",
                    "吃完起来晃晃",
                    "午休十分钟，猫先示范",
            },
            { // afternoon, 14-18
                    "下午茶时间到",
                    "再撑一会就下班",
                    "起来倒杯水吧",
                    "窗外的光正好",
            },
            { // evening, 19-23
                    "下班了，猫在等你",
                    "晚饭想好吃什么了吗",
                    "晚上早点回",
                    "今晚也辛苦啦",
            },
            { // deep night, 0-4
                    "还没睡啊，猫先睡了",
                    "屏幕调暗点",
                    "深夜了，手机放下吧",
                    "晚安，明天见",
            },
    };

    private static final String KAOMOJI = "(=^･ω･^=)";

    private Overlay overlay;
    private int overlayProgram;
    private int overlayPos;
    private int overlayUv;
    private int overlayScaleOffset;
    private int overlayTex;

    private Bitmap bandBitmap;
    private Canvas bandCanvas;
    private Paint bandPaint;
    /** Reused rather than allocated per repaint; the GL thread owns both. */
    private final Path cardOutline = new Path();
    private final Path cardFold = new Path();
    private int bandTexture;
    private int bandWidth;
    private int bandHeight;
    private boolean bandDirty = true;
    /** The second the strip was last repainted for; repainting twice in one
     * second is wasted texture upload, so the paint decision is per second. */
    private long bandSecond = -1;
    private final String[] bandRendered = new String[Overlay.MAX_LINES];
    private final String[] bandScratch = new String[Overlay.MAX_LINES];
    /** Rows the line currently being drawn was broken into. */
    private final String[] bandRows = new String[MAX_TEXT_ROWS];
    private final Calendar bandCalendar = Calendar.getInstance();
    /** GL-thread-only cache of fonts loaded from the panel's own filesystem. */
    private static final Map<String, Typeface> TYPEFACE_CACHE = new HashMap<>();

    /**
     * The faces bundled with this app, handed over by the activity.
     *
     * <p>The panel's own fonts are a rounded Chinese face and a sans-serif, and
     * neither matches artwork of a cat at a keyboard. These travel in the APK,
     * so they look the same on every unit regardless of what the vendor put in
     * `/system/fonts`. Not loaded here: `Typeface.createFromAsset` reads the
     * whole file, and the GL thread has no time for it.
     */
    private Typeface[] bundledTypefaces;

    /** The same order the host's `Font::Bundled(n)` indexes into. */
    private static final String[] BUNDLED_FACE_ASSETS = {
            "fonts/ZCOOLKuaiLe-Regular.ttf",
            "fonts/ZCOOLQingKeHuangYou-Regular.ttf",
            "fonts/MaShanZheng-Regular.ttf",
            "fonts/ZCOOLXiaoWei-Regular.ttf",
    };

    /** Loads the bundled faces; a missing file leaves that slot empty. */
    static Typeface[] loadBundledTypefaces(android.content.res.AssetManager assets) {
        Typeface[] faces = new Typeface[BUNDLED_FACE_ASSETS.length];
        for (int i = 0; i < BUNDLED_FACE_ASSETS.length; i++) {
            try {
                faces[i] = Typeface.createFromAsset(assets, BUNDLED_FACE_ASSETS[i]);
            } catch (RuntimeException error) {
                Log.w(TAG, "no bundled face at " + BUNDLED_FACE_ASSETS[i]);
            }
        }
        return faces;
    }

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

    /** The bundled playful faces; `null` falls back to the panel's default. */
    void setBundledTypefaces(Typeface[] faces) {
        this.bundledTypefaces = faces;
    }

    @Override
    public void onSurfaceCreated(GL10 unused, EGLConfig config) {
        discardPreviousContext();

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

        overlayProgram = buildProgram(VERTEX_SHADER, OVERLAY_FRAGMENT);
        overlayPos = GLES20.glGetAttribLocation(overlayProgram, "aPos");
        overlayUv = GLES20.glGetAttribLocation(overlayProgram, "aUv");
        overlayScaleOffset = GLES20.glGetUniformLocation(overlayProgram, "uScaleOffset");
        overlayTex = GLES20.glGetUniformLocation(overlayProgram, "uModelTex");

        blankTexture = createBlankTexture();

        Log.i(TAG, "GL_VERSION=" + GLES20.glGetString(GLES20.GL_VERSION));
        Log.i(TAG, "GL_RENDERER=" + GLES20.glGetString(GLES20.GL_RENDERER));
        int[] maxTexture = new int[1];
        GLES20.glGetIntegerv(GLES20.GL_MAX_TEXTURE_SIZE, maxTexture, 0);
        Log.i(TAG, "GL_MAX_TEXTURE_SIZE=" + maxTexture[0]);

        windowStart = System.nanoTime();
        frames = 0;
    }

    /**
     * Forgets everything the previous GL context owned.
     *
     * <p>Leaving the panel and coming back tears the context down — {@code
     * GLSurfaceView} does that on every pause — and a new context is a new
     * namespace. The texture names this renderer remembers mean nothing in it:
     * binding one samples whatever that number happens to be now, which is
     * usually nothing. The model then drew as black while the clock and the
     * banner came back, because those are repainted on every resize and so
     * happened to be rebuilt. Frames keep arriving either way, so the panel
     * looked connected and only the cat was gone.
     *
     * <p>The host only sends textures at the head of a connection, so a lost
     * context is only recoverable by dialling again. Nothing is dialled on the
     * first creation, when there is no previous context to forget.
     */
    private void discardPreviousContext() {
        boolean hadResources = textures.size() > 0 || keyTextures.size() > 0 || scene != null;
        textures.clear();
        keyTextures.clear();
        // The old framebuffers and their attachments died with the context; the
        // names are stale and deleting them would address the new namespace.
        maskTargets.clear();
        releaseBand();
        if (scene != null) {
            scene.recycle();
            scene = null;
        }
        haveFrame = false;
        cleared = false;
        if (hadResources) {
            Log.i(TAG, "GL context replaced: discarding "
                    + "the model and asking the host for it again");
            client.reconnect();
        }
    }

    @Override
    public void onSurfaceChanged(GL10 unused, int width, int height) {
        viewWidth = Math.max(1, width);
        viewHeight = Math.max(1, height);
        GLES20.glViewport(0, 0, viewWidth, viewHeight);
        // Mask targets are the size of the surface, so a resize invalidates them.
        releaseMaskTargets();
        // The band bitmap is the width of the surface too, and the old texture
        // belonged to the previous GL context if there was one.
        releaseBand();
        Log.i(TAG, "SURFACE=" + width + "x" + height);
    }

    @Override
    public void onDrawFrame(GL10 unused) {
        Scene incoming = client.takeScene();
        if (incoming != null) {
            applyScene(incoming);
        }
        Overlay incomingOverlay = client.takeOverlay();
        if (incomingOverlay != null) {
            overlay = incomingOverlay;
            bandSecond = -1;
            bandDirty = true;
        }
        if (client.takeCleared()) {
            haveFrame = false;
            cleared = true;
        }
        if (client.copyFrameInto(frame)) {
            haveFrame = true;
            cleared = false;
            lastFrameNanos = System.nanoTime();
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
        if (haveFrame && !cleared) {
            renderOverlay();
        }
        maybeSendPreview();
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

    /** Rounds a wanted target size up to {@link #MASK_TARGET_STEP}. */
    private static int quantizeTarget(int pixels, int limit) {
        if (pixels <= 0) {
            return 0;
        }
        int rounded = ((pixels + MASK_TARGET_STEP - 1) / MASK_TARGET_STEP) * MASK_TARGET_STEP;
        return Math.min(rounded, limit);
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

    /**
     * The clip-space transform the D3D11 and Metal backends use, verbatim.
     *
     * <p>The rectangle it frames is the canvas, not the model's own bounds: the
     * desk and the key images are drawn across the canvas, and framing the moving
     * parts alone pushes them off the edge of the panel.
     */
    private void modelTransform() {
        float minX = scene != null ? scene.canvasMinX : frame.minX;
        float maxX = scene != null ? scene.canvasMaxX : frame.maxX;
        float minY = scene != null ? scene.canvasMinY : frame.minY;
        float maxY = scene != null ? scene.canvasMaxY : frame.maxY;
        float modelWidth = Math.max(maxX - minX, 1e-6f);
        float modelHeight = Math.max(maxY - minY, 1e-6f);
        float centerX = (minX + maxX) * 0.5f;
        float centerY = (minY + maxY) * 0.5f;
        float modelAspect = modelWidth / modelHeight;
        float viewportAspect = (float) viewWidth / (float) viewHeight;

        float scaleX = 2f / modelWidth;
        float scaleY = 2f / modelHeight;
        if (viewportAspect > modelAspect) {
            scaleX *= modelAspect / viewportAspect;
        } else {
            scaleY *= viewportAspect / modelAspect;
        }

        // The band sits under the picture. While there is black to spare the
        // picture keeps the centre it has always had — with the default band and
        // the 1.72:1 canvas this is exactly the old layout — and only a band
        // tall enough to reach the picture pushes it up, never down. A band
        // that covers the surface pushes nothing: it is a layer, and the whole
        // point of it is that the picture stays where it is.
        int band = overlay != null ? Math.max(0, overlay.bandPx) : 0;
        float shiftPx = 0f;
        if (band > 0 && !isLayerBand(band)) {
            float picturePxH = modelHeight * scaleY * 0.5f * viewHeight;
            float limitCenter = (viewHeight - band) - picturePxH * 0.5f - BAND_BOTTOM_MARGIN_PX;
            shiftPx = Math.min(0f, limitCenter - viewHeight * 0.5f);
        }

        float offsetX = -centerX * scaleX;
        if (frame.mirror) {
            scaleX = -scaleX;
            offsetX = -offsetX;
        }
        scaleOffset[0] = scaleX;
        scaleOffset[1] = scaleY;
        scaleOffset[2] = offsetX;
        scaleOffset[3] = -centerY * scaleY - 2f * shiftPx / viewHeight;
    }

    /**
     * Draws the strip against the bottom edge of the panel.
     *
     * <p>The strip is one bitmap the width of the surface and the height of the
     * band, uploaded as a texture whenever its rendered text actually changed.
     * The repaint decision is per wall-clock second: a template with seconds
     * repaints every second, one without repaints once a minute, and a static
     * banner repaints never. Between repaints the draw is one textured quad.
     */
    private void renderOverlay() {
        Overlay strip = overlay;
        if (strip == null || strip.bandPx <= 0 || strip.lines.length == 0) {
            return;
        }
        // A layer can never be taller than the surface it lies over, and it has
        // to be exactly that tall for the line positions to be screen
        // coordinates rather than offsets from a bottom edge.
        int bandPx = isLayerBand(strip.bandPx) ? viewHeight : strip.bandPx;
        if (bandBitmap == null || bandWidth != viewWidth || bandHeight != bandPx) {
            rebuildBand(bandPx);
        }
        long second = System.currentTimeMillis() / 1000L;
        if (bandDirty || second != bandSecond) {
            bandSecond = second;
            // A dirty band means the description itself changed — a new colour,
            // a new alignment, a new line count — and none of those show up in
            // the formatted text, so the repaint has to be forced rather than
            // decided by a string comparison.
            if (repaintBand(strip, bandDirty)) {
                GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, bandTexture);
                GLUtils.texImage2D(GLES20.GL_TEXTURE_2D, 0, bandBitmap, 0);
                bandDirty = false;
            }
        }
        if (bandTexture == 0) {
            return;
        }
        GLES20.glUseProgram(overlayProgram);
        GLES20.glUniform4f(overlayScaleOffset, 1f, 1f, 0f, 0f);
        GLES20.glUniform1i(overlayTex, 0);
        GLES20.glEnable(GLES20.GL_BLEND);
        blendNormal();
        GLES20.glDisable(GLES20.GL_CULL_FACE);
        GLES20.glActiveTexture(GLES20.GL_TEXTURE0);
        GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, bandTexture);
        // Clip-space y grows upwards, so the band's top edge is above -1.
        float bandTop = -1f + 2f * bandHeight / viewHeight;
        drawQuad(overlayPos, overlayUv, -1f, 1f, -1f, bandTop);
    }

    /**
     * Whether a band of this height covers the picture instead of sitting under
     * it.
     *
     * <p>Threshold rather than a flag because the height is the only thing the
     * wire carries, and because the two modes differ by an order of magnitude:
     * a bar under the artwork never needs six tenths of the surface, and a layer
     * over it has no reason to be shorter.
     */
    private boolean isLayerBand(int bandPx) {
        return bandPx >= viewHeight * BAND_LAYER_SHARE;
    }

    /** Allocates the band texture and its bitmap for the current surface. */
    private void rebuildBand(int bandPx) {
        releaseBandTexture();
        if (bandBitmap != null) {
            bandBitmap.recycle();
        }
        bandWidth = viewWidth;
        bandHeight = bandPx;
        bandBitmap = Bitmap.createBitmap(bandWidth, bandHeight, Bitmap.Config.ARGB_8888);
        bandCanvas = new Canvas(bandBitmap);
        if (bandPaint == null) {
            bandPaint = new Paint();
        }
        int[] textures = new int[1];
        GLES20.glGenTextures(1, textures, 0);
        bandTexture = textures[0];
        GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, bandTexture);
        GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_MIN_FILTER, GLES20.GL_LINEAR);
        GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_MAG_FILTER, GLES20.GL_LINEAR);
        GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_WRAP_S, GLES20.GL_CLAMP_TO_EDGE);
        GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_WRAP_T, GLES20.GL_CLAMP_TO_EDGE);
        bandDirty = true;
    }

    private void releaseBand() {
        releaseBandTexture();
        if (bandBitmap != null) {
            bandBitmap.recycle();
            bandBitmap = null;
        }
        bandCanvas = null;
        bandWidth = 0;
        bandHeight = 0;
        bandDirty = true;
    }

    private void releaseBandTexture() {
        if (bandTexture != 0) {
            int[] textures = {bandTexture};
            GLES20.glDeleteTextures(1, textures, 0);
            bandTexture = 0;
        }
    }

    /**
     * Rasterises the strip. Returns whether anything changed; the caller only
     * uploads the texture then.
     *
     * <p>{@code force} repaints regardless of the text, for the case where the
     * description changed but the strings it produces did not.
     */
    private boolean repaintBand(Overlay strip, boolean force) {
        bandCalendar.setTimeInMillis(System.currentTimeMillis());
        boolean changed = force;
        for (int i = 0; i < strip.lines.length; i++) {
            bandScratch[i] = formatTemplate(strip.lines[i].text, bandCalendar);
            String previous = bandRendered[i];
            if (!bandScratch[i].equals(previous)) {
                changed = true;
            }
        }
        if (!changed) {
            return false;
        }
        // Cleared to transparent, not filled: the top of the strip has to let
        // the picture through, and `Mode.SRC` with the background colour would
        // paint it solid before the fade is drawn over it.
        bandCanvas.drawColor(Color.TRANSPARENT, PorterDuff.Mode.CLEAR);
        // A layer draws no background at all. It covers the whole surface, so
        // filling it would black out the cat; the text carries its own shadow
        // and that is the only thing that reaches the glass.
        boolean layer = isLayerBand(strip.bandPx);
        int fadeHeight = layer ? 0 : Math.round(bandHeight * BAND_FADE_SHARE);
        if (fadeHeight > 0) {
            // Same colour at both ends, only the alpha moves: the fade has to
            // land on exactly the background underneath it, or the seam shows.
            LinearGradient fade = new LinearGradient(
                    0f, 0f, 0f, fadeHeight,
                    strip.background & 0x00FFFFFF, strip.background,
                    Shader.TileMode.CLAMP);
            bandPaint.setShader(fade);
            bandPaint.setStyle(Paint.Style.FILL);
            bandCanvas.drawRect(0f, 0f, bandWidth, fadeHeight, bandPaint);
            bandPaint.setShader(null);
        }
        if (!layer) {
            bandPaint.setColor(strip.background);
            bandPaint.setStyle(Paint.Style.FILL);
            bandCanvas.drawRect(0f, fadeHeight, bandWidth, bandHeight, bandPaint);
            if (strip.divider) {
                bandPaint.setColor(BAND_DIVIDER_COLOR);
                bandPaint.setTextAlign(Paint.Align.LEFT);
                bandCanvas.drawRect(0f, 0f, bandWidth, 1f, bandPaint);
            }
        } else {
            // Under the text, because it exists to be read from.
            drawClockCard();
        }
        // Everything below is expressed in surface coordinates, so a bar at the
        // bottom and a layer over the whole picture go through the same maths.
        float bandTop = viewHeight - bandHeight;
        float scale = cardScale();
        for (int i = 0; i < strip.lines.length; i++) {
            Overlay.Line line = strip.lines[i];
            bandRendered[i] = bandScratch[i];
            bandPaint.setTypeface(typefaceFor(line));
            bandPaint.setTextSize(line.sizePx);
            bandPaint.setColor(line.color);
            bandPaint.setAntiAlias(true);
            // A soft drop shadow, because without a card these lines sit over
            // the artwork rather than on flat black. It is what keeps the clock
            // legible where the desk shows through behind it.
            bandPaint.setShadowLayer(3f, 0f, 2f, 0x99000000);
            Paint.FontMetrics metrics = bandPaint.getFontMetrics();

            float centerY;
            if (layer && line.align == Overlay.ALIGN_CARD) {
                centerY = CARD_Y * scale + line.centerY * scale;
                // Centred in the card, both lines: two strings of different
                // widths sharing a left edge read as a table, and a table is
                // not what a note looks like.
                float x = (CARD_X + CARD_WIDTH * 0.5f) * scale;
                bandPaint.setTextAlign(Paint.Align.CENTER);
                // Ink on paper: a shadow here reads as a dirty smudge rather
                // than as depth.
                bandPaint.clearShadowLayer();
                // The card is a fixed box: whatever does not fit inside it is
                // the host's problem, so these two lines are never broken.
                bandCanvas.drawText(
                        bandRendered[i],
                        x,
                        centerY - (metrics.ascent + metrics.descent) * 0.5f,
                        bandPaint);
                continue;
            }
            centerY = bandTop + line.centerY;
            int align = line.align;
            if (align == Overlay.ALIGN_CARD) {
                // No card, so the near-black chosen to sit on white would
                // vanish into the picture.
                align = Overlay.ALIGN_LEFT;
                bandPaint.setColor(CARD_TEXT_ON_ARTWORK);
            }
            bandPaint.setTextAlign(paintAlign(align));
            // The strip line is the only one that can be arbitrarily long, and
            // it sits where the circle is already narrow, so it is the one that
            // gets broken into rows instead of running under the bezel.
            float rowHeight = line.sizePx * TEXT_LINE_HEIGHT_SHARE;
            int rows = fitRows(bandRendered[i], line.rows, centerY, rowHeight);
            float firstRowY = centerY - (rows - 1) * rowHeight * 0.5f;
            drawBannerRows(line, align, rows, firstRowY, rowHeight, metrics);
        }
        return true;
    }

    /**
     * Breaks one strip line into rows that each fit the chord at their own
     * height.
     *
     * <p>A round panel narrows as it descends, so a long banner cannot be split
     * into equal rows: the lower a row sits, the less width it has. The fit is
     * tried with one row first, then two, then three, and every trial cuts each
     * row against the circle where that row will actually be. One row
     * reproduces the old single-line placement exactly, so nothing that already
     * fitted moves by a pixel.
     *
     * <p>{@code preference} is what the host asked for. One row is honoured
     * even when the text does not fit — the bezel then clips it, which is the
     * honest reading of "keep it on one line". Two rows are laid out only if
     * both halves fit; a sentence that needs more falls back to breaking by
     * width rather than being drawn under the bezel.
     *
     * <p>Returns the number of rows and leaves their text in {@link #bandRows}.
     * The rows are returned in reading order and the whole block stays centred
     * on the line's own centre line.
     */
    private int fitRows(String text, int preference, float centerY, float rowHeight) {
        if (preference == Overlay.ROWS_ONE) {
            bandRows[0] = text;
            clearRows(1);
            return 1;
        }
        if (preference == Overlay.ROWS_TWO) {
            float available = chordWidthAt(centerY) - 2f * BAND_TEXT_EDGE_PADDING_PX;
            // A line that already sits comfortably in one row is left alone:
            // "two rows" is permission to break a long sentence, not an order
            // to break every one.
            if (bandPaint.measureText(text) <= available * TWO_ROW_FILL_SHARE) {
                bandRows[0] = text;
                clearRows(1);
                return 1;
            }
            int split = splitIndex(text);
            if (split > 0 && split < text.length()) {
                String top = text.substring(0, split).trim();
                String bottom = text.substring(split).trim();
                if (fits(top, centerY - rowHeight * 0.5f)
                        && fits(bottom, centerY + rowHeight * 0.5f)) {
                    bandRows[0] = top;
                    bandRows[1] = bottom;
                    clearRows(2);
                    return 2;
                }
            }
        }
        for (int rows = 1; rows <= MAX_TEXT_ROWS; rows++) {
            float firstRowY = centerY - (rows - 1) * rowHeight * 0.5f;
            int cursor = 0;
            int filled = 0;
            for (int r = 0; r < rows; r++) {
                if (cursor >= text.length()) {
                    break;
                }
                float room = chordWidthAt(firstRowY + r * rowHeight)
                        - 2f * BAND_TEXT_EDGE_PADDING_PX;
                int take = takeRow(text, cursor, room);
                bandRows[r] = text.substring(cursor, cursor + take).trim();
                // A cut that lands on a space leaves the space to the row that
                // already had it; the next row must not start with one.
                cursor += take;
                while (cursor < text.length() && text.charAt(cursor) == ' ') {
                    cursor++;
                }
                filled = r + 1;
            }
            if (cursor >= text.length() || rows == MAX_TEXT_ROWS) {
                clearRows(filled);
                return Math.max(1, filled);
            }
        }
        return 1;
    }

    /** Empties the row slots from {@code from} on, so a stale row is never drawn. */
    private void clearRows(int from) {
        for (int r = Math.max(0, from); r < MAX_TEXT_ROWS; r++) {
            bandRows[r] = "";
        }
    }

    /** Whether {@code text} fits the chord at this height, with its inset. */
    private boolean fits(String text, float screenY) {
        return bandPaint.measureText(text) <= chordWidthAt(screenY)
                - 2f * BAND_TEXT_EDGE_PADDING_PX;
    }

    // ---------------------------------------------------------------------
    // Banner dressing. The codes come in the line's style halfword; the
    // drawing happens here, around the same fitted rows the plain path lays
    // out, so every style inherits the bezel-aware break for free.
    // ---------------------------------------------------------------------

    private static final int BANNER_INK = 0xFF3A3A40;
    private static final int BANNER_AMBER = 0xFFF0A83C;
    private static final int BANNER_PINK = 0xFFF27C7C;
    private static final int BANNER_BUBBLE = 0xFFFAFAF8;
    private static final int BANNER_PILL = 0xFF282B31;
    private static final int BANNER_PILL_EDGE = 0xFF3A3E46;
    private static final int[] RAINBOW = {
            0xFFFF99AA, 0xFFFFB76E, 0xFFFFDC6E, 0xFF96E196,
            0xFF78CDF0, 0xFFBEA0F5, 0xFFF596C8,
    };
    private static final int BANNER_JELLY = 0xFFFFC4D2;
    private static final int BANNER_JELLY_SHINE = 0xFFFFE4EB;
    private static final int BANNER_JELLY_TEXT = 0xFF963E54;
    private static final int BANNER_BRASS = 0xFFC6A05A;
    private static final int BANNER_BRASS_DARK = 0xFF967236;
    private static final int BANNER_BRASS_LINE = 0xFFE8CA84;
    private static final int BANNER_BRASS_TEXT = 0xFF4A3612;
    private static final int BANNER_BRASS_SHINE = 0xFFF0D89E;
    private static final int BANNER_WOOD = 0xFF946840;
    private static final int BANNER_WOOD_DARK = 0xFF684628;
    private static final int BANNER_WOOD_GRAIN = 0xFF7E5632;
    private static final int BANNER_WOOD_TEXT = 0xFFF6E8CE;
    private static final int BANNER_ENAMEL = 0xFFF9F9F7;
    private static final int BANNER_ENAMEL_EDGE = 0xFFD6DAE2;
    private static final int BANNER_ENAMEL_LINE = 0xFF34447A;
    private static final int BANNER_ENAMEL_TEXT = 0xFF2A3A74;
    private static final int BANNER_SWEET = 0xFFFFD6E0;
    private static final int BANNER_SWEET_EDGE = 0xFFE2829E;
    private static final int BANNER_SWEET_HEART = 0xFFEE7896;
    private static final int BANNER_SWEET_TEXT = 0xFFC44668;
    private static final int BANNER_GOLD_PLATE = 0xFF1E1E22;
    private static final int BANNER_GOLD_EDGE = 0xFF0A0A0C;
    private static final int BANNER_GOLD_LINE = 0xFFCCAC5E;
    private static final int BANNER_GOLD_TEXT = 0xFFE2C270;
    private static final int BANNER_TAG = 0xFFF6F0E0;
    private static final int BANNER_TAG_EDGE = 0xFFC4B28E;
    private static final int BANNER_TAG_TEXT = 0xFF604A2E;
    private static final int BANNER_TAG_STRING = 0xFFB4B0A8;

    private final Path bannerPath = new Path();

    /**
     * Draws the fitted rows of a strip line, dressed however the style asks.
     *
     * <p>Plain is the path the strip has always taken. Surface styles draw a
     * plate or bubble behind the block and clear the drop shadow first, the
     * way the clock card does — a shadow under a filled surface reads as a
     * smudge. Ink styles only change the colour or the per-character layout.
     */
    private void drawBannerRows(Overlay.Line line, int align, int rows,
            float firstRowY, float rowHeight, Paint.FontMetrics metrics) {
        if (line.style == Overlay.STYLE_PLAIN || line.style == Overlay.STYLE_LOW) {
            drawPlainRows(align, rows, firstRowY, rowHeight, metrics, line.color, 1f);
            return;
        }

        float size = line.sizePx;
        float widest = 0f;
        for (int r = 0; r < rows; r++) {
            widest = Math.max(widest, bandPaint.measureText(bandRows[r]));
        }
        float cx = bandWidth * 0.5f;
        float blockH = rows * rowHeight;
        float blockTop = firstRowY - blockH * 0.5f;
        float blockBottom = firstRowY + blockH * 0.5f;
        float boxLeft = cx - widest * 0.5f - size * 1.05f;
        float boxRight = cx + widest * 0.5f + size * 1.05f;
        float boxTop = blockTop - size * 0.5f;
        float boxBottom = blockBottom + size * 0.5f;

        switch (line.style) {
            case Overlay.STYLE_FADE: {
                drawPlainRows(align, rows, firstRowY, rowHeight, metrics, line.color, 0.51f);
                break;
            }
            case Overlay.STYLE_PILL: {
                bandPaint.clearShadowLayer();
                for (int r = 0; r < rows; r++) {
                    if (bandRows[r].length() == 0) {
                        continue;
                    }
                    float rowY = firstRowY + r * rowHeight;
                    float w = bandPaint.measureText(bandRows[r]);
                    float h = rowHeight * 0.86f;
                    bandPaint.setStyle(Paint.Style.FILL);
                    bandPaint.setColor(BANNER_PILL);
                    bandCanvas.drawRoundRect(cx - w * 0.5f - size * 0.9f, rowY - h * 0.5f,
                            cx + w * 0.5f + size * 0.9f, rowY + h * 0.5f,
                            h * 0.5f, h * 0.5f, bandPaint);
                    bandPaint.setStyle(Paint.Style.STROKE);
                    bandPaint.setColor(BANNER_PILL_EDGE);
                    bandPaint.setStrokeWidth(1f);
                    bandCanvas.drawRoundRect(cx - w * 0.5f - size * 0.9f, rowY - h * 0.5f,
                            cx + w * 0.5f + size * 0.9f, rowY + h * 0.5f,
                            h * 0.5f, h * 0.5f, bandPaint);
                }
                bandPaint.setStyle(Paint.Style.FILL);
                drawPlainRows(align, rows, firstRowY, rowHeight, metrics, line.color, 1f);
                break;
            }
            case Overlay.STYLE_TWO_TONE: {
                for (int r = 0; r < rows; r++) {
                    String row = bandRows[r];
                    if (row.length() == 0) {
                        continue;
                    }
                    float rowY = firstRowY + r * rowHeight;
                    int cut = firstPunctuation(row);
                    if (cut <= 0) {
                        bandPaint.setColor(line.color);
                        bandCanvas.drawText(row, paintX(align, rowY, row),
                                baselineOf(rowY, metrics), bandPaint);
                    } else {
                        String head = row.substring(0, cut);
                        float headW = bandPaint.measureText(head);
                        float startX = rowStartX(align, rowY, row);
                        bandPaint.setTextAlign(Paint.Align.LEFT);
                        bandPaint.setColor(BANNER_AMBER);
                        bandCanvas.drawText(head, startX, baselineOf(rowY, metrics), bandPaint);
                        bandPaint.setColor(line.color);
                        bandCanvas.drawText(row.substring(cut), startX + headW,
                                baselineOf(rowY, metrics), bandPaint);
                        bandPaint.setTextAlign(paintAlign(align));
                    }
                }
                break;
            }
            case Overlay.STYLE_PAWS: {
                drawPlainRows(align, rows, firstRowY, rowHeight, metrics, line.color, 1f);
                float unit = size * 0.28f;
                float rowY = widestRowY(rows, firstRowY, rowHeight);
                float gap = widest * 0.5f + size * 1.1f;
                drawBannerPaw(cx - gap, rowY, unit, BANNER_PINK);
                drawBannerPaw(cx + gap, rowY, unit, BANNER_PINK);
                break;
            }
            case Overlay.STYLE_BUBBLE: {
                bandPaint.clearShadowLayer();
                bandPaint.setStyle(Paint.Style.FILL);
                bandPaint.setColor(BANNER_BUBBLE);
                float radius = size * 0.8f;
                bandCanvas.drawRoundRect(boxLeft, boxTop, boxRight, boxBottom,
                        radius, radius, bandPaint);
                // The tail leans up-left, toward where the cat sits.
                bannerPath.reset();
                bannerPath.moveTo(cx - size * 2.3f, boxTop + 2f);
                bannerPath.lineTo(cx - size * 0.5f, boxTop + 2f);
                bannerPath.lineTo(cx - size * 1.7f, boxTop - size * 1.15f);
                bannerPath.close();
                bandCanvas.drawPath(bannerPath, bandPaint);
                drawPlainRows(align, rows, firstRowY, rowHeight, metrics, BANNER_INK, 1f);
                break;
            }
            case Overlay.STYLE_RAINBOW: {
                for (int r = 0; r < rows; r++) {
                    String row = bandRows[r];
                    if (row.length() == 0) {
                        continue;
                    }
                    float rowY = firstRowY + r * rowHeight;
                    float x = rowStartX(align, rowY, row);
                    for (int c = 0; c < row.length(); c++) {
                        bandPaint.setColor(RAINBOW[c % RAINBOW.length]);
                        String glyph = String.valueOf(row.charAt(c));
                        bandCanvas.drawText(glyph, x, baselineOf(rowY, metrics), bandPaint);
                        x += bandPaint.measureText(glyph);
                    }
                }
                break;
            }
            case Overlay.STYLE_WAVE: {
                for (int r = 0; r < rows; r++) {
                    String row = bandRows[r];
                    if (row.length() == 0) {
                        continue;
                    }
                    float rowY = firstRowY + r * rowHeight;
                    float x = rowStartX(align, rowY, row);
                    for (int c = 0; c < row.length(); c++) {
                        float dy = (c % 2 == 0) ? -size * 0.18f : size * 0.18f;
                        bandPaint.setColor(line.color);
                        String glyph = String.valueOf(row.charAt(c));
                        bandCanvas.drawText(glyph, x, baselineOf(rowY, metrics) + dy, bandPaint);
                        x += bandPaint.measureText(glyph);
                    }
                }
                break;
            }
            case Overlay.STYLE_KAOMOJI: {
                drawPlainRows(align, rows, firstRowY, rowHeight, metrics, line.color, 1f);
                float rowY = widestRowY(rows, firstRowY, rowHeight);
                float small = size * 0.58f;
                bandPaint.setTextSize(small);
                bandPaint.setTextAlign(Paint.Align.LEFT);
                float gap = widest * 0.5f + small * 1.6f;
                bandPaint.setColor(BANNER_AMBER);
                float base = baselineOf(rowY, metrics);
                bandCanvas.drawText(KAOMOJI, cx - gap - bandPaint.measureText(KAOMOJI),
                        base, bandPaint);
                bandCanvas.drawText(KAOMOJI, cx + gap, base, bandPaint);
                bandPaint.setTextSize(size);
                bandPaint.setTextAlign(paintAlign(align));
                break;
            }
            case Overlay.STYLE_JELLY: {
                bandPaint.clearShadowLayer();
                plateBox(boxLeft, boxTop, boxRight, boxBottom, size * 1.2f,
                        BANNER_SWEET_EDGE, BANNER_JELLY, BANNER_JELLY_SHINE, size * 0.14f);
                // The highlight that makes it read as jelly rather than paint.
                bandPaint.setStyle(Paint.Style.STROKE);
                bandPaint.setColor(BANNER_JELLY_SHINE);
                bandPaint.setStrokeWidth(size * 0.14f);
                bandPaint.setStrokeCap(Paint.Cap.ROUND);
                bandCanvas.drawArc(boxLeft + size * 0.5f, boxTop + size * 0.32f,
                        boxLeft + size * 2.4f, boxBottom - size * 0.9f, 40f, 90f, false, bandPaint);
                bandPaint.setStyle(Paint.Style.FILL);
                drawPlainRows(align, rows, firstRowY, rowHeight, metrics, BANNER_JELLY_TEXT, 1f);
                break;
            }
            case Overlay.STYLE_BRASS: {
                bandPaint.clearShadowLayer();
                plateBox(boxLeft, boxTop, boxRight, boxBottom, size * 0.34f,
                        BANNER_BRASS_DARK, BANNER_BRASS, BANNER_BRASS_LINE, size * 0.07f);
                plateScrews(boxLeft, boxTop, boxRight, boxBottom,
                        0xFF765A2A, 0xFF564018);
                for (int r = 0; r < rows; r++) {
                    String row = bandRows[r];
                    if (row.length() == 0) {
                        continue;
                    }
                    float rowY = firstRowY + r * rowHeight;
                    float base = baselineOf(rowY, metrics);
                    // Engraving: a light lip under a dark cut.
                    bandPaint.setColor(BANNER_BRASS_SHINE);
                    bandCanvas.drawText(row, paintX(align, rowY, row), base + size * 0.05f,
                            bandPaint);
                    bandPaint.setColor(BANNER_BRASS_TEXT);
                    bandCanvas.drawText(row, paintX(align, rowY, row), base, bandPaint);
                }
                break;
            }
            case Overlay.STYLE_WOOD: {
                bandPaint.clearShadowLayer();
                plateBox(boxLeft, boxTop, boxRight, boxBottom, size * 0.42f,
                        BANNER_WOOD_DARK, BANNER_WOOD, 0, 0);
                bandPaint.setStyle(Paint.Style.STROKE);
                bandPaint.setColor(BANNER_WOOD_GRAIN);
                bandPaint.setStrokeWidth(size * 0.06f);
                for (float gy = boxTop + size * 0.42f; gy < boxBottom - size * 0.2f;
                        gy += size * 0.34f) {
                    bandCanvas.drawLine(boxLeft + size * 0.55f, gy,
                            boxRight - size * 0.55f, gy + size * 0.06f, bandPaint);
                }
                bandPaint.setStyle(Paint.Style.FILL);
                drawPlainRows(align, rows, firstRowY, rowHeight, metrics, BANNER_WOOD_TEXT, 1f);
                break;
            }
            case Overlay.STYLE_ENAMEL: {
                bandPaint.clearShadowLayer();
                plateBox(boxLeft, boxTop, boxRight, boxBottom, size * 0.34f,
                        BANNER_ENAMEL_EDGE, BANNER_ENAMEL, BANNER_ENAMEL_LINE, size * 0.07f);
                bandPaint.setStyle(Paint.Style.FILL);
                bandPaint.setColor(0xFF202228);
                float holeY = (boxTop + boxBottom) * 0.5f;
                bandCanvas.drawCircle(boxLeft + size * 0.62f, holeY, size * 0.14f, bandPaint);
                bandCanvas.drawCircle(boxRight - size * 0.62f, holeY, size * 0.14f, bandPaint);
                drawPlainRows(align, rows, firstRowY, rowHeight, metrics, BANNER_ENAMEL_TEXT, 1f);
                break;
            }
            case Overlay.STYLE_PINK_SWEET: {
                bandPaint.clearShadowLayer();
                plateBox(boxLeft, boxTop, boxRight, boxBottom, size * 0.62f,
                        BANNER_SWEET_EDGE, BANNER_SWEET, 0xFFFFFFFF, size * 0.07f);
                float heartY = (boxTop + boxBottom) * 0.5f;
                drawBannerHeart(boxLeft + size * 0.78f, heartY, size * 0.3f, BANNER_SWEET_HEART);
                drawBannerHeart(boxRight - size * 0.78f, heartY, size * 0.3f, BANNER_SWEET_HEART);
                drawPlainRows(align, rows, firstRowY, rowHeight, metrics, BANNER_SWEET_TEXT, 1f);
                break;
            }
            case Overlay.STYLE_BLACK_GOLD: {
                bandPaint.clearShadowLayer();
                plateBox(boxLeft, boxTop, boxRight, boxBottom, size * 0.34f,
                        BANNER_GOLD_EDGE, BANNER_GOLD_PLATE, BANNER_GOLD_LINE, size * 0.07f);
                drawPlainRows(align, rows, firstRowY, rowHeight, metrics, BANNER_GOLD_TEXT, 1f);
                break;
            }
            case Overlay.STYLE_TAG: {
                bandPaint.clearShadowLayer();
                float midY = (blockTop + blockBottom) * 0.5f;
                bandCanvas.save();
                bandCanvas.rotate(5f, cx, midY);
                // The string comes down from the desk edge the tag hangs on.
                bandPaint.setStyle(Paint.Style.STROKE);
                bandPaint.setColor(BANNER_TAG_STRING);
                bandPaint.setStrokeWidth(size * 0.09f);
                bandCanvas.drawLine(cx + size * 0.7f, boxTop - size * 1.9f, cx,
                        boxTop + size * 0.5f, bandPaint);
                bandPaint.setStyle(Paint.Style.FILL);
                plateBox(boxLeft, boxTop, boxRight, boxBottom, size * 0.46f,
                        BANNER_TAG_EDGE, BANNER_TAG, 0, 0);
                bandPaint.setColor(0xFF202226);
                bandCanvas.drawCircle(cx, boxTop + size * 0.5f, size * 0.22f, bandPaint);
                drawPlainRows(align, rows, firstRowY, rowHeight, metrics, BANNER_TAG_TEXT, 1f);
                bandCanvas.restore();
                break;
            }
            default:
                drawPlainRows(align, rows, firstRowY, rowHeight, metrics, line.color, 1f);
                break;
        }
        bandPaint.setAlpha(255);
    }

    /** The undecorated path: one draw per row, in the line's own colour. */
    private void drawPlainRows(int align, int rows, float firstRowY, float rowHeight,
            Paint.FontMetrics metrics, int color, float alpha) {
        // Colour first, alpha second: setColor would overwrite the alpha that
        // setAlpha stored, and the faded style would come out opaque.
        bandPaint.setColor(color);
        bandPaint.setAlpha(Math.round(255f * alpha));
        for (int r = 0; r < rows; r++) {
            String row = bandRows[r];
            if (row.length() == 0) {
                continue;
            }
            float rowY = firstRowY + r * rowHeight;
            bandCanvas.drawText(
                    row,
                    paintX(align, rowY, row),
                    baselineOf(rowY, metrics),
                    bandPaint);
        }
        bandPaint.setAlpha(255);
    }

    /**
     * Where a row starts when the caller has to place pieces along it — the
     * two-tone halves and the per-character styles. The plain path anchors by
     * the paint's align; these styles need the row's left edge instead.
     */
    private float rowStartX(int align, float rowY, String row) {
        float anchor = paintX(align, rowY, row);
        switch (paintAlign(align)) {
            case LEFT:
                return anchor;
            case RIGHT:
                return anchor - bandPaint.measureText(row);
            default:
                return anchor - bandPaint.measureText(row) * 0.5f;
        }
    }

    private float baselineOf(float rowY, Paint.FontMetrics metrics) {
        return rowY - (metrics.ascent + metrics.descent) * 0.5f;
    }

    private float widestRowY(int rows, float firstRowY, float rowHeight) {
        int best = 0;
        float bestW = -1f;
        for (int r = 0; r < rows; r++) {
            float w = bandPaint.measureText(bandRows[r]);
            if (w > bestW) {
                bestW = w;
                best = r;
            }
        }
        return firstRowY + best * rowHeight;
    }

    /** Index of the first real punctuation mark, or -1 when the row has none. */
    private static int firstPunctuation(String row) {
        for (int i = 0; i < row.length(); i++) {
            char c = row.charAt(i);
            if (c != ' ' && c != '\t' && isBreakAfter(c)) {
                return i;
            }
        }
        return -1;
    }

    /** A filled plate with a darker edge and an optional inner rule line. */
    private void plateBox(float left, float top, float right, float bottom, float radius,
            int edge, int fill, int line, float lineWidth) {
        bandPaint.setStyle(Paint.Style.FILL);
        bandPaint.setColor(edge);
        bandCanvas.drawRoundRect(left, top, right, bottom, radius, radius, bandPaint);
        bandPaint.setColor(fill);
        bandCanvas.drawRoundRect(left + 3f, top + 3f, right - 3f, bottom - 3f,
                Math.max(2f, radius - 3f), Math.max(2f, radius - 3f), bandPaint);
        if (line != 0) {
            bandPaint.setStyle(Paint.Style.STROKE);
            bandPaint.setColor(line);
            bandPaint.setStrokeWidth(lineWidth);
            float inset = Math.max(4f, lineWidth * 2.6f);
            bandCanvas.drawRoundRect(left + inset, top + inset, right - inset, bottom - inset,
                    Math.max(2f, radius - inset * 0.6f), Math.max(2f, radius - inset * 0.6f),
                    bandPaint);
        }
    }

    /** One slotted screw in each corner of the box. */
    private void plateScrews(float left, float top, float right, float bottom,
            int body, int slot) {
        bandPaint.setStyle(Paint.Style.FILL);
        float inset = 14f;
        for (float sx : new float[]{left + inset, right - inset}) {
            for (float sy : new float[]{top + inset, bottom - inset}) {
                bandPaint.setColor(body);
                bandCanvas.drawCircle(sx, sy, 4.5f, bandPaint);
                bandPaint.setColor(slot);
                bandCanvas.drawLine(sx - 2.8f, sy - 2.8f, sx + 2.8f, sy + 2.8f, bandPaint);
            }
        }
    }

    /** The pink paw print, sized by {@code unit} the way the card's paw is. */
    private void drawBannerPaw(float cx, float cy, float unit, int color) {
        bandPaint.setStyle(Paint.Style.FILL);
        bandPaint.setColor(color);
        bandCanvas.drawCircle(cx, cy + 2.6f * unit, 4.4f * unit, bandPaint);
        bandCanvas.drawCircle(cx - 4.6f * unit, cy - 1.6f * unit, 2.1f * unit, bandPaint);
        bandCanvas.drawCircle(cx, cy - 3.6f * unit, 2.4f * unit, bandPaint);
        bandCanvas.drawCircle(cx + 4.6f * unit, cy - 1.6f * unit, 2.1f * unit, bandPaint);
    }

    private void drawBannerHeart(float cx, float cy, float s, int color) {
        bandPaint.setStyle(Paint.Style.FILL);
        bandPaint.setColor(color);
        bandCanvas.drawCircle(cx - 0.42f * s, cy - 0.3f * s, 0.55f * s, bandPaint);
        bandCanvas.drawCircle(cx + 0.42f * s, cy - 0.3f * s, 0.55f * s, bandPaint);
        bannerPath.reset();
        bannerPath.moveTo(cx - 0.92f * s, cy - 0.12f * s);
        bannerPath.lineTo(cx + 0.92f * s, cy - 0.12f * s);
        bannerPath.lineTo(cx, cy + 0.95f * s);
        bannerPath.close();
        bandCanvas.drawPath(bannerPath, bandPaint);
    }

    /**
     * Where to break a line into two halves.
     *
     * <p>Preference is a space or a punctuation mark, because a sentence broken
     * there still reads as a sentence. Among those the one nearest the middle
     * wins, so the two rows end up about the same length. A break that would
     * leave one row under a third of the line is refused: at that point the
     * middle of the text is the better guess, and an unbalanced pair reads as a
     * mistake rather than as a pause.
     *
     * <p>Returns {@code 0} when the text is too short to be worth splitting.
     */
    private static int splitIndex(String text) {
        int length = text.length();
        if (length < 2) {
            return 0;
        }
        int middle = length / 2;
        int low = Math.max(1, length / 3);
        int high = Math.min(length - 1, length - length / 3);
        int best = -1;
        int bestDistance = Integer.MAX_VALUE;
        for (int i = low; i <= high; i++) {
            if (!isBreakAfter(text.charAt(i - 1))) {
                continue;
            }
            int distance = Math.abs(i - middle);
            if (distance < bestDistance) {
                bestDistance = distance;
                best = i;
            }
        }
        return best > 0 ? best : middle;
    }

    private static boolean isBreakAfter(char c) {
        switch (c) {
            case ' ':
            case '\t':
            case ',':
            case '.':
            case '!':
            case '?':
            case ';':
            case ':':
            case '，':
            case '。':
            case '、':
            case '！':
            case '？':
            case '；':
            case '：':
            case '…':
                return true;
            default:
                return false;
        }
    }

    /**
     * How many characters starting at {@code cursor} fit in {@code room}.
     *
     * <p>Always at least one: a row too narrow even for a single glyph still
     * has to consume something, or the caller never advances. Latin words are
     * kept whole when a space is reachable behind the cut, because a banner
     * broken mid-word reads as a rendering bug rather than as a break.
     */
    private int takeRow(String text, int cursor, float room) {
        int remaining = text.length() - cursor;
        String tail = text.substring(cursor);
        int take = (int) bandPaint.breakText(tail, true, room, null);
        if (take <= 0) {
            return 1;
        }
        if (take >= remaining) {
            return remaining;
        }
        if (isLatinLetter(text.charAt(cursor + take - 1))
                && isLatinLetter(text.charAt(cursor + take))) {
            int back = cursor + take - 1;
            while (back > cursor && text.charAt(back) != ' ') {
                back--;
            }
            if (back > cursor) {
                take = back - cursor;
            }
        }
        return take;
    }

    private static boolean isLatinLetter(char c) {
        return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c == '\'';
    }

    /**
     * Width of the circle's chord at a screen height.
     *
     * <p>Zero outside the circle, which is what makes a row pushed past the
     * bezel collapse to a single character instead of drawing off the glass.
     */
    private float chordWidthAt(float screenY) {
        float radius = Math.min(viewWidth, viewHeight) * 0.5f;
        float dy = Math.abs(screenY - viewHeight * 0.5f);
        if (dy >= radius) {
            return 0f;
        }
        return 2f * (float) Math.sqrt(radius * radius - dy * dy);
    }

    /** Scales the card's 800px geometry to the surface actually on offer. */
    private float cardScale() {
        return Math.min(viewWidth, viewHeight) / CARD_REFERENCE_PX;
    }

    /** Paints the clock card's paper, before anything is written on it. */
    private void drawClockCard() {
        float scale = cardScale();
        float left = CARD_X * scale;
        float top = CARD_Y * scale;
        float right = left + CARD_WIDTH * scale;
        float bottom = top + CARD_HEIGHT * scale;
        float radius = CARD_RADIUS * scale;

        bandPaint.setShader(null);
        bandPaint.setStyle(Paint.Style.FILL);
        bandPaint.setAntiAlias(true);
        bandPaint.clearShadowLayer();

        bandPaint.setColor(CARD_FILL);
        bandCanvas.drawRoundRect(left, top, right, bottom, radius, radius, bandPaint);

        // The header is a plain rectangle, so it has to be cut to the card's own
        // outline or its square corners would poke out through the rounding.
        bandCanvas.save();
        cardOutline.reset();
        cardOutline.addRoundRect(left, top, right, bottom, radius, radius, Path.Direction.CW);
        bandCanvas.clipPath(cardOutline);

        float headerBottom = top + CARD_HEADER_HEIGHT * scale;
        bandPaint.setColor(CARD_HEADER_FILL);
        bandCanvas.drawRect(left, top, right, headerBottom, bandPaint);
        bandPaint.setColor(CARD_HAIRLINE);
        bandCanvas.drawRect(left, headerBottom, right,
                headerBottom + Math.max(1f, scale), bandPaint);
        bandCanvas.restore();

        drawCardFold((left + right) * 0.5f,
                top + CARD_HEADER_HEIGHT * scale * 0.5f + scale, scale);
        drawCardPaw(right - 16f * scale, top + 12f * scale, scale);
    }

    /** The small upward chevron in the middle of the header. */
    private void drawCardFold(float centerX, float centerY, float scale) {
        float halfWidth = 5f * scale;
        float height = 3f * scale;
        cardFold.reset();
        cardFold.moveTo(centerX - halfWidth, centerY + height);
        cardFold.lineTo(centerX, centerY - height);
        cardFold.lineTo(centerX + halfWidth, centerY + height);

        bandPaint.setShader(null);
        bandPaint.setStyle(Paint.Style.STROKE);
        bandPaint.setColor(CARD_FOLD_COLOR);
        bandPaint.setStrokeWidth(Math.max(1.2f, 1.6f * scale));
        bandPaint.setStrokeCap(Paint.Cap.ROUND);
        bandPaint.setStrokeJoin(Paint.Join.ROUND);
        bandPaint.setAntiAlias(true);
        bandCanvas.drawPath(cardFold, bandPaint);
        bandPaint.setStyle(Paint.Style.FILL);
    }

    /** The pink paw in the header's right corner: three toes over one pad. */
    private void drawCardPaw(float centerX, float centerY, float scale) {
        bandPaint.setShader(null);
        bandPaint.setStyle(Paint.Style.FILL);
        bandPaint.setColor(CARD_PAW_COLOR);
        bandPaint.setAntiAlias(true);
        float unit = scale;
        bandCanvas.drawCircle(centerX, centerY + 2.6f * unit, 4.4f * unit, bandPaint);
        bandCanvas.drawCircle(centerX - 4.6f * unit, centerY - 1.6f * unit, 2.1f * unit, bandPaint);
        bandCanvas.drawCircle(centerX, centerY - 3.6f * unit, 2.4f * unit, bandPaint);
        bandCanvas.drawCircle(centerX + 4.6f * unit, centerY - 1.6f * unit, 2.1f * unit, bandPaint);
    }

    /** Substitutes the clock placeholders. Unknown fields pass through untouched. */
    private static String formatTemplate(String template, Calendar calendar) {
        if (template.indexOf('{') < 0) {
            return template;
        }
        StringBuilder out = new StringBuilder(template.length() + 8);
        int at = 0;
        while (at < template.length()) {
            int start = template.indexOf('{', at);
            if (start < 0) {
                out.append(template, at, template.length());
                break;
            }
            int end = template.indexOf('}', start + 1);
            if (end < 0) {
                out.append(template, at, template.length());
                break;
            }
            out.append(template, at, start);
            appendField(out, template.substring(start + 1, end), calendar);
            at = end + 1;
        }
        return out.toString();
    }

    private static void appendField(StringBuilder out, String field, Calendar calendar) {
        switch (field) {
            case "yyyy":
                out.append(pad(calendar.get(Calendar.YEAR), 4));
                break;
            case "M":
                out.append(calendar.get(Calendar.MONTH) + 1);
                break;
            case "MM":
                out.append(pad(calendar.get(Calendar.MONTH) + 1, 2));
                break;
            case "dd":
                out.append(pad(calendar.get(Calendar.DAY_OF_MONTH), 2));
                break;
            case "HH":
                out.append(pad(calendar.get(Calendar.HOUR_OF_DAY), 2));
                break;
            case "mm":
                out.append(pad(calendar.get(Calendar.MINUTE), 2));
                break;
            case "ss":
                out.append(pad(calendar.get(Calendar.SECOND), 2));
                break;
            case "W":
                out.append(WEEKDAYS[calendar.get(Calendar.DAY_OF_WEEK) - 1]);
                break;
            case "random":
            case "random:":
                // One line per hour, derived from the clock so that reconnects
                // and restarts land on the same one.
                out.append(randomBanner("", calendar));
                break;
            default:
                if (field.startsWith("random:")) {
                    out.append(randomBanner(field.substring(7), calendar));
                    break;
                }
                out.append('{').append(field).append('}');
                break;
        }
    }

    /**
     * The sentence the random marker stands for this hour.
     *
     * <p>The spec after the colon names the pools in rotation — `heal`, `fun`,
     * `poem`, `moe`, `time` — and carries the person's own sentences after the
     * first pipe, one per pipe. An empty spec means everything, which is what
     * the bare `{random}` a previous host wrote has always meant. The hour
     * picks the line out of the merge, so the rotation keeps running while the
     * host is closed and lands where it left off after a restart.
     */
    private static String randomBanner(String spec, Calendar calendar) {
        ArrayList<String> merged = new ArrayList<>();
        String poolsPart = spec;
        int pipe = spec.indexOf('|');
        if (pipe >= 0) {
            poolsPart = spec.substring(0, pipe);
            for (String own : spec.substring(pipe + 1).split("\\|")) {
                String line = own.trim();
                if (!line.isEmpty()) {
                    merged.add(line);
                }
            }
        }
        if (poolsPart.isEmpty()) {
            addPool(merged, POOL_HEAL);
            addPool(merged, POOL_FUN);
            addPool(merged, POOL_POEM);
            addPool(merged, POOL_MOE);
            addPool(merged, timePool(calendar));
        } else {
            for (String id : poolsPart.split(",")) {
                switch (id.trim()) {
                    case "heal":
                        addPool(merged, POOL_HEAL);
                        break;
                    case "fun":
                        addPool(merged, POOL_FUN);
                        break;
                    case "poem":
                        addPool(merged, POOL_POEM);
                        break;
                    case "moe":
                        addPool(merged, POOL_MOE);
                        break;
                    case "time":
                        addPool(merged, timePool(calendar));
                        break;
                    default:
                        break;
                }
            }
        }
        if (merged.isEmpty()) {
            return POOL_HEAL[0];
        }
        int count = merged.size();
        long hours = calendar.getTimeInMillis() / 3_600_000L;
        int slot = (int) (((hours % count) + count) % count);
        return merged.get(slot);
    }

    private static void addPool(ArrayList<String> merged, String[] pool) {
        merged.addAll(Arrays.asList(pool));
    }

    /** Which bucket of the day this hour belongs to. */
    private static String[] timePool(Calendar calendar) {
        switch (calendar.get(Calendar.HOUR_OF_DAY)) {
            case 5: case 6: case 7: case 8: case 9: case 10:
                return TIME_POOLS[0];
            case 11: case 12: case 13:
                return TIME_POOLS[1];
            case 14: case 15: case 16: case 17: case 18:
                return TIME_POOLS[2];
            case 19: case 20: case 21: case 22: case 23:
                return TIME_POOLS[3];
            default:
                return TIME_POOLS[4];
        }
    }

    private static String pad(int value, int width) {
        String raw = Integer.toString(value);
        if (raw.length() >= width) {
            return raw;
        }
        StringBuilder out = new StringBuilder(width);
        for (int i = raw.length(); i < width; i++) {
            out.append('0');
        }
        return out.append(raw).toString();
    }

    private Typeface typefaceFor(Overlay.Line line) {
        boolean bold = line.weight == Overlay.WEIGHT_BOLD;
        int style = bold ? Typeface.BOLD : Typeface.NORMAL;
        if (line.font >= Overlay.FONT_BUNDLED) {
            int index = line.font - Overlay.FONT_BUNDLED;
            Typeface face = bundledTypefaces != null && index < bundledTypefaces.length
                    ? bundledTypefaces[index]
                    : null;
            // An index this build does not carry is a host that is newer than
            // the app, which is the normal case right after an update.
            if (face != null) {
                return face;
            }
            return Typeface.create(Typeface.SANS_SERIF, style);
        }
        switch (line.font) {
            case Overlay.FONT_ROUNDED: {
                String path = bold ? ROUNDED_BOLD_FONT_PATH : ROUNDED_FONT_PATH;
                Typeface face = loadTypeface(path);
                if (face != null) {
                    return face;
                }
                return Typeface.create(Typeface.SANS_SERIF, style);
            }
            case Overlay.FONT_MONO:
                return Typeface.create(Typeface.MONOSPACE, style);
            default:
                return Typeface.create(Typeface.SANS_SERIF, style);
        }
    }

    private static Typeface loadTypeface(String path) {
        Typeface cached = TYPEFACE_CACHE.get(path);
        if (cached != null) {
            return cached;
        }
        try {
            Typeface face = Typeface.createFromFile(path);
            TYPEFACE_CACHE.put(path, face);
            return face;
        } catch (RuntimeException error) {
            // The firmware's decorative face is a nice-to-have; sans-serif is
            // always there and reads fine.
            Log.w(TAG, "no typeface at " + path);
            return null;
        }
    }

    private static Paint.Align paintAlign(int align) {
        switch (align) {
            case Overlay.ALIGN_LEFT:
                return Paint.Align.LEFT;
            case Overlay.ALIGN_RIGHT:
                return Paint.Align.RIGHT;
            default:
                return Paint.Align.CENTER;
        }
    }

    /**
     * Where a line starts or ends, kept inside the circle.
     *
     * <p>A strip sized to hold three centred lines in the bottom of a round
     * panel uses a fraction of its width, because the shape itself narrows to
     * nothing at the bottom edge. Rather than shrink the text until it fits the
     * narrowest row, geometry decides the inset: the chord at the row's own
     * height is the space that actually exists there, so a line pinned to the
     * left edge sits as far out as the glass allows instead of hugging the
     * centre.
     *
     * <p>{@code screenY} is the line's centre in surface coordinates, which is
     * the same axis the chord is measured on.
     */
    private float paintX(int align, float screenY, String text) {
        if (align == Overlay.ALIGN_CENTER) {
            return bandWidth * 0.5f;
        }
        float centreX = viewWidth * 0.5f;
        // Half the text plus a little air, so the glyphs clear the bezel rather
        // than the text box's origin clearing it.
        float inset = bandPaint.measureText(text) * 0.5f + BAND_TEXT_EDGE_PADDING_PX;
        float edge = Math.max(0f, centreX - chordWidthAt(screenY) * 0.5f) + inset;
        if (edge > centreX) {
            // A row so close to the edge that even the inset does not fit: fall
            // back to the fixed margin rather than drawing outside the circle.
            edge = centreX;
        }
        if (align == Overlay.ALIGN_LEFT) {
            return Math.max(BAND_TEXT_MARGIN_PX, edge);
        }
        return Math.min(bandWidth - BAND_TEXT_MARGIN_PX, bandWidth - edge);
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
            int requiredWidth = quantizeTarget(Math.min(viewWidth,
                    (int) Math.ceil(bounds[2]) - (int) Math.floor(bounds[0]) + 2),
                    viewWidth);
            int requiredHeight = quantizeTarget(Math.min(viewHeight,
                    (int) Math.ceil(bounds[3]) - (int) Math.floor(bounds[1]) + 2),
                    viewHeight);
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

            float clipX = x * scaleOffset[0] + scaleOffset[2];
            float clipY = y * scaleOffset[1] + scaleOffset[3];
            float pixelX = (clipX * 0.5f + 0.5f) * viewWidth;
            float pixelY = (clipY * 0.5f + 0.5f) * viewHeight;

            // Hash the position in whole pixels, not the raw float bits. A clip
            // is rasterised at MASK_SCALE of the panel, so a movement smaller
            // than one texel cannot change a single pixel of the picture it
            // produces — but hashing the raw bits made the idle breathing
            // animation invalidate every clip on every frame, and re-rasterising
            // them is exactly what kept this device at 42 fps instead of the
            // panel's 57.6. Whole-pixel quantisation leaves a margin of a couple
            // of texels, which is still below what the eye can see moving.
            hash = mix(hash, Math.round(pixelX / MASK_HASH_QUANTUM));
            hash = mix(hash, Math.round(pixelY / MASK_HASH_QUANTUM));

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
                        scene.canvasMinX, scene.canvasMaxX,
                        scene.canvasMinY, scene.canvasMaxY);
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

    /**
     * Captures the panel and hands the JPEG to the client.
     *
     * <p>The host asked for this because the picture only exists here: the
     * publisher never rasterises. Reading the framebuffer costs a few milliseconds
     * on this GPU, which is why the interval is measured rather than tuned, and
     * why nothing is captured when no host is listening.
     */
    private void maybeSendPreview() {
        boolean requested = client.isPreviewRequested();
        if (!requested || viewWidth <= 0 || viewHeight <= 0) {
            previewWasRequested = false;
            return;
        }
        long now = System.nanoTime();
        // The host now asks for a snapshot just after something changed and stops
        // asking as soon as one lands, so the first capture after a request must
        // not wait for the interval — otherwise the request expires unanswered and
        // the page keeps showing a picture from before the edit.
        boolean first = !previewWasRequested;
        previewWasRequested = true;
        if (!first && now - lastPreviewNanos < PREVIEW_INTERVAL_NANOS) {
            return;
        }
        lastPreviewNanos = now;

        int width = viewWidth;
        int height = viewHeight;
        if (previewPixels == null || previewPixels.length != width * height) {
            previewPixels = new int[width * height];
            previewRotated = new int[width * height];
        }

        GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, 0);
        java.nio.IntBuffer target = java.nio.IntBuffer.wrap(previewPixels);
        GLES20.glReadPixels(0, 0, width, height, GLES20.GL_RGBA,
                GLES20.GL_UNSIGNED_BYTE, target);

        // glReadPixels is bottom-up and returns RGBA in memory order, while an
        // ARGB_8888 bitmap is top-down with alpha in the high byte: one pass does
        // the flip and the channel swap together.
        for (int y = 0; y < height; y++) {
            int source = (height - 1 - y) * width;
            int destination = y * width;
            for (int x = 0; x < width; x++) {
                int rgba = previewPixels[source + x];
                previewRotated[destination + x] = (rgba & 0xFF00FF00)
                        | ((rgba & 0x000000FF) << 16)
                        | ((rgba >> 16) & 0x000000FF);
            }
        }

        Bitmap full = Bitmap.createBitmap(previewRotated, width, height,
                Bitmap.Config.ARGB_8888);
        Bitmap scaled = Bitmap.createScaledBitmap(full, PREVIEW_SIZE, PREVIEW_SIZE, true);
        java.io.ByteArrayOutputStream out = new java.io.ByteArrayOutputStream(32 * 1024);
        scaled.compress(Bitmap.CompressFormat.JPEG, PREVIEW_QUALITY, out);
        if (scaled != full) {
            scaled.recycle();
        }
        full.recycle();
        client.sendPreview(out.toByteArray());
    }

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

        // The publisher streams continuously, so silence is a dead link rather
        // than an idle cat. Keeping the last frame on the panel is better than
        // going black, but the picture has to stop claiming to be live: otherwise
        // a broken tunnel looks exactly like a cat that happens to be sitting
        // still, and there is nothing on the panel to diagnose it with.
        long silentMillis = haveFrame ? (now - lastFrameNanos) / 1_000_000L : Long.MAX_VALUE;
        boolean live = cleared || (haveFrame && scene != null && silentMillis < STALE_AFTER_MS);
        String age = silentMillis == Long.MAX_VALUE
                ? "no frame yet"
                : String.format("frame %.1fs old", silentMillis / 1000f);
        String text = String.format("%s  %.1f fps  %.0fx%.0f  masks skipped %d  keys %d/%d  %s",
                live ? "CatScreen" : "waiting", fps, (float) viewWidth, (float) viewHeight,
                skippedMasks, keysDrawn, keyMisses, age);
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
