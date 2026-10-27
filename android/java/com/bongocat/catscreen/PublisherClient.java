package com.bongocat.catscreen;

import android.content.Context;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.util.Log;

import java.io.BufferedInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.Socket;

/**
 * Keeps one TCP connection to the publisher alive and hands finished frames to
 * the render thread.
 *
 * <p>Two channels carry the stream. The control socket — this class — carries
 * the handshake, the textures, the strip, the audio library and every command,
 * because those are things a dropped byte has to be recovered from. Frames come
 * over datagrams instead, through {@link UdpFrameChannel}: a frame is a complete
 * state, so the newest one is the only one worth having, and a TCP stream of them
 * over Wi-Fi queues the backlog in the access point until the picture on the
 * panel is exactly as old as the network is slow.
 *
 * <p>Parsing happens here, never on the render thread. The two are decoupled by a
 * single copied frame: the render thread is allowed to be slower than the stream
 * and simply misses frames, which is what keeps the picture in step with the
 * newest state rather than falling behind by seconds.
 */
final class PublisherClient implements Runnable, UdpFrameChannel.Sink {

    interface Listener {
        void onStatus(String text, boolean live);
    }

    private static final String TAG = "CatScreen";
    /** The publisher the reverse tunnel points at. */
    private static final String DEFAULT_ADDRESS = "127.0.0.1:7391";
    private static final int DEFAULT_PORT = 7391;
    private static final int CONNECT_TIMEOUT_MS = 2000;
    private static final int RETRY_DELAY_MS = 750;
    /**
     * How many refusals in a row before the address is doubted.
     *
     * <p>The host restarts and the tunnel drops for a moment, so one refusal is
     * noise. Two is the point where retrying the same address starts costing
     * more than asking the network where the host went.
     */
    private static final int DISCOVER_AFTER_FAILURES = 2;
    /** How long one discovery round listens for an answer. */
    private static final int DISCOVERY_TIMEOUT_MS = 1200;

    /**
     * The publisher ticks once a second even when the picture is still, so a
     * socket that stays silent is a socket whose tunnel is gone. Without this the
     * reader blocks in {@code read()} forever after the adb server is restarted:
     * the device's end of the reverse tunnel is never closed, so no end-of-stream
     * arrives and the cat freezes while the thread sits there. The value is a
     * multiple of the host's tick so a single dropped tick is not a disconnect.
     */
    private static final int READ_TIMEOUT_MS = 12000;

    /** How often the host is told what arrived. */
    private static final long STATS_INTERVAL_MS = 2000;

    /**
     * How far behind the host's frame number the datagrams may fall before the
     * channel is judged broken.
     *
     * <p>The host marks one frame a second, so this is measured in received
     * frames rather than in time: a link that drops the occasional datagram still
     * delivers most frames, and one that delivers none is unambiguous.
     */
    private static final long MARK_LAG_LIMIT = 10;

    /** How long after a bad mark the datagram channel is declared dead. */
    private static final long DATAGRAM_SILENCE_MS = 1500;

    /** How long the datagram channel is left alone before being tried again. */
    private static final long DATAGRAM_RETRY_MS = 30_000;

    /** How many times a failed datagram channel is retried in one connection. */
    private static final int DATAGRAM_RETRIES = 3;

    /** Where the last chosen address is remembered across restarts. */
    private static final String PREFERENCES = "link";
    private static final String PREFERENCE_ADDRESS = "address";

    private final Object lock = new Object();
    private final Frame shared = new Frame();

    /** The context, kept for the address preference. */
    private final Context context;

    /**
     * Where the publisher lives.
     *
     * <p>Not final: the host moves the panel between the network and the cable
     * while it is running, and the change arrives as a message on the very
     * socket it is about to replace.
     */
    private volatile String address;

    private final UdpFrameChannel datagrams;
    private final AudioPlayer audio;

    PublisherClient(Context context, String address) {
        this.context = context;
        String remembered = context
                .getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
                .getString(PREFERENCE_ADDRESS, null);
        this.address = firstNonEmpty(address, remembered, DEFAULT_ADDRESS);
        this.datagrams = new UdpFrameChannel(this);
        this.audio = new AudioPlayer(context);
    }

    private static String firstNonEmpty(String... values) {
        for (String value : values) {
            if (value != null && !value.trim().isEmpty()) {
                return value.trim();
            }
        }
        return DEFAULT_ADDRESS;
    }

    private Scene pendingScene;
    private Overlay pendingOverlay;
    private boolean hasFrame;
    private boolean cleared;

    private volatile boolean running = true;
    private volatile Listener listener;
    private volatile Socket socket;

    /**
     * Whether the host is displaying snapshots. The render thread reads this every
     * frame, so it is a plain volatile flag rather than a queue: the newest value
     * is the only one that matters.
     */
    private volatile boolean previewRequested;
    /**
     * Guards the output side against two snapshots overlapping. The capture runs
     * on the render thread, which must never interleave half a JPEG into the
     * stream the reader is parsing on the other thread.
     */
    private final Object sendLock = new Object();

    /**
     * The newest frame already handed to the render thread.
     *
     * <p>Two channels deliver frames and either may be ahead of the other: while
     * the datagram path is warming up the host is still writing frames to the
     * control socket, and those are older than the datagrams already assembled.
     * Sequence numbers are monotonic within a model generation, so this is what
     * keeps a stale picture from overwriting a fresh one.
     */
    private long lastGeneration = -1;
    private long lastSequence = -1;

    /**
     * What the panel was last told, so the sixty frames a second that all say
     * "live" do not each become a log line and a message to the UI thread.
     */
    private final Object publishLock = new Object();
    private String lastPublished;
    private boolean lastPublishedLive;

    private volatile long lastMarkAt;
    private volatile long lastMarkSequence = -1;
    private volatile long lastDatagramFrameAt;
    private volatile long lastDatagramSequence = -1;
    private volatile boolean datagramSuspended;
    private volatile long datagramSuspendedAt;
    private volatile int datagramFailures;

    void setListener(Listener listener) {
        this.listener = listener;
    }

    /** The address this client is trying to reach, for the host's own logging. */
    String address() {
        return address;
    }

    /**
     * Moves to another publisher address.
     *
     * <p>Closing the socket is what makes the change take effect: the reader is
     * blocked in {@code read()} on the old connection, and the loop above only
     * dials again once that call has returned. The address is written before the
     * close so the retry cannot read a half-applied pair.
     */
    void switchAddress(String target) {
        if (target == null || target.trim().isEmpty()) {
            return;
        }
        String next = target.trim();
        if (next.equals(address)) {
            return;
        }
        Log.i(TAG, "moving from " + address + " to " + next);
        address = next;
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
                .edit()
                .putString(PREFERENCE_ADDRESS, next)
                .apply();
        closeQuietly(socket);
        socket = null;
    }

    /** The newest decoded resources, or null when none arrived since the last call. */
    Scene takeScene() {
        synchronized (lock) {
            Scene scene = pendingScene;
            pendingScene = null;
            return scene;
        }
    }

    /** The newest strip description, or null when none arrived since the last call. */
    Overlay takeOverlay() {
        synchronized (lock) {
            Overlay overlay = pendingOverlay;
            pendingOverlay = null;
            return overlay;
        }
    }

    /** Copies the newest frame into {@code target}; false when nothing arrived. */
    boolean copyFrameInto(Frame target) {
        synchronized (lock) {
            if (!hasFrame) {
                return false;
            }
            target.copyFrom(shared);
            hasFrame = false;
            return true;
        }
    }

    /** Whether the publisher has hidden the model since the last call. */
    boolean takeCleared() {
        synchronized (lock) {
            boolean value = cleared;
            cleared = false;
            return value;
        }
    }

    /** Whether the host wants snapshots of the panel right now. */
    boolean isPreviewRequested() {
        return previewRequested;
    }

    /** Whether frames are currently arriving as datagrams. */
    boolean isDatagramChannelActive() {
        return datagrams.isRegistered() && !datagramSuspended;
    }

    /**
     * Sends one snapshot to the host.
     *
     * <p>Called from the render thread at a low rate, so a blocking write is
     * acceptable: the socket's own send timeout bounds it, and the frame stream in
     * the other direction is unaffected. A failed write is dropped silently; the
     * next snapshot will try again, and the reader will notice a dead link first.
     */
    void sendPreview(byte[] jpeg) {
        if (jpeg == null || jpeg.length == 0) {
            return;
        }
        byte[] payload = new byte[8 + jpeg.length];
        writeLe32(payload, 0, Protocol.VERSION);
        writeLe32(payload, 4, jpeg.length);
        System.arraycopy(jpeg, 0, payload, 8, jpeg.length);
        send(Protocol.KIND_PREVIEW_FRAME, payload);
    }

    /** Sends one message on the control socket, or drops it when there is none. */
    private void send(int kind, byte[] payload) {
        Socket current = socket;
        if (current == null) {
            return;
        }
        synchronized (sendLock) {
            try {
                OutputStream out = current.getOutputStream();
                byte[] header = new byte[5];
                header[0] = (byte) kind;
                int length = payload.length;
                header[1] = (byte) (length & 0xFF);
                header[2] = (byte) ((length >>> 8) & 0xFF);
                header[3] = (byte) ((length >>> 16) & 0xFF);
                header[4] = (byte) ((length >>> 24) & 0xFF);
                out.write(header);
                out.write(payload);
                out.flush();
            } catch (IOException error) {
                Log.w(TAG, "message dropped: " + error.getMessage());
            }
        }
    }

    private static void writeLe32(byte[] target, int offset, int value) {
        target[offset] = (byte) (value & 0xFF);
        target[offset + 1] = (byte) ((value >>> 8) & 0xFF);
        target[offset + 2] = (byte) ((value >>> 16) & 0xFF);
        target[offset + 3] = (byte) ((value >>> 24) & 0xFF);
    }

    void stop() {
        running = false;
        datagrams.stop();
        audio.close();
        closeQuietly(socket);
        socket = null;
    }

    @Override
    public void run() {
        long lastStats = 0;
        int failures = 0;
        while (running) {
            Socket connection = new Socket();
            InetSocketAddress endpoint = parseAddress(address);
            try {
                connection.connect(endpoint, CONNECT_TIMEOUT_MS);
                connection.setTcpNoDelay(true);
                connection.setSoTimeout(READ_TIMEOUT_MS);
                socket = connection;
                failures = 0;
                publish("connected", false);
                // The datagram channel is per connection: a new control socket
                // means the host may be a different process on a different port.
                //
                // It is not opened over the cable. A reverse tunnel forwards TCP
                // only, so a datagram sent to the loopback would be captured by
                // this panel's own stack and never reach the host; frames would
                // then stop arriving while every TCP counter looked healthy.
                if (endpoint.getAddress() != null && endpoint.getAddress().isLoopbackAddress()) {
                    datagrams.stop();
                } else {
                    datagrams.open(endpoint.getHostString(), datagramPort(endpoint.getPort()));
                }
                read(connection);
                publish("publisher closed the connection", false);
            } catch (IOException error) {
                if (running) {
                    publish("waiting for the publisher: " + error.getMessage(), false);
                    Log.w(TAG, "connection lost: " + error.getMessage());
                }
            } catch (Throwable error) {
                // A parser bug must not take this thread with it. The panel is the
                // program's only output, so a reader that dies silently is
                // indistinguishable from a cat that stopped moving.
                if (running) {
                    publish("reader failed: " + error, false);
                    Log.e(TAG, "reader failed", error);
                }
            } finally {
                closeQuietly(connection);
                socket = null;
            }
            if (!running) {
                break;
            }
            failures++;
            // After a couple of refusals, stop trusting the address and ask the
            // network. The path this panel was told about is the path it is
            // stuck on, and the host cannot correct it over a link that is down:
            // a panel left on a loopback port after the cable went away would
            // otherwise retry forever at an address nothing answers.
            if (failures >= DISCOVER_AFTER_FAILURES
                    && failures % DISCOVER_AFTER_FAILURES == 0) {
                InetSocketAddress found =
                        HostDiscovery.find(endpoint.getPort(), DISCOVERY_TIMEOUT_MS);
                if (found != null) {
                    String next = found.getAddress().getHostAddress() + ":" + found.getPort();
                    if (!next.equals(address)) {
                        Log.i(TAG, "discovery moved us from " + address + " to " + next);
                        switchAddress(next);
                        failures = 0;
                    }
                }
            }
            sleep(RETRY_DELAY_MS);
        }
    }

    private void read(Socket connection) throws IOException {
        InputStream raw = new BufferedInputStream(connection.getInputStream(), 64 * 1024);
        Protocol.Reader reader = new Protocol.Reader(raw);
        boolean live = false;
        long lastStats = System.currentTimeMillis();

        // A new connection is a new host session, and the sequence watermark
        // belongs to that session rather than to this panel. The host numbers
        // its frames from one per process, so a host that restarts and reloads
        // the same model arrives with the same generation and numbers the panel
        // has already seen — every frame is then a "duplicate", nothing is
        // drawn, and the panel sits on "loading the model" while the link is
        // perfectly healthy. Starting each connection from nothing makes the
        // first frame of a session the newest frame of a session.
        synchronized (lock) {
            lastGeneration = -1;
            lastSequence = -1;
        }
        // The same reasoning applies to what the panel is told: after a drop the
        // first report of the new session has to reach the screen even when it
        // repeats the words from the end of the last one.
        synchronized (publishLock) {
            lastPublished = null;
        }

        while (running && reader.next()) {
            switch (reader.kind) {
                case Protocol.KIND_HELLO:
                    readHello(reader);
                    break;
                case Protocol.KIND_RESOURCES:
                    publish("loading the model…", false);
                    Scene scene = readScene(reader);
                    synchronized (lock) {
                        Scene previous = pendingScene;
                        pendingScene = scene;
                        if (previous != null) {
                            previous.recycle();
                        }
                    }
                    break;
                case Protocol.KIND_FRAME:
                    // The control socket still carries frames until the host has
                    // heard from this device, and again if it stops doing so.
                    if (applyFrame(reader.payloadCopy())) {
                        live = true;
                        publish("live", true);
                    }
                    break;
                case Protocol.KIND_CLEAR:
                    if (reader.u32() != Protocol.VERSION) {
                        throw new IOException("clear carries an unknown version");
                    }
                    clearFrame();
                    break;
                case Protocol.KIND_OVERLAY:
                    Overlay overlay = Overlay.read(reader);
                    synchronized (lock) {
                        pendingOverlay = overlay;
                    }
                    break;
                case Protocol.KIND_AUDIO_LIBRARY:
                    readAudioLibrary(reader);
                    break;
                case Protocol.KIND_AUDIO_PLAY:
                    if (reader.u32() != Protocol.VERSION) {
                        throw new IOException("audio play carries an unknown version");
                    }
                    audio.play(reader.u32(), reader.f32());
                    break;
                case Protocol.KIND_AUDIO_STOP:
                    if (reader.u32() != Protocol.VERSION) {
                        throw new IOException("audio stop carries an unknown version");
                    }
                    audio.stop();
                    break;
                case Protocol.KIND_FRAME_MARK:
                    lastMarkAt = System.currentTimeMillis();
                    lastMarkSequence = reader.u64();
                    break;
                case Protocol.KIND_RELINK:
                    if (reader.u32() != Protocol.VERSION) {
                        throw new IOException("relink carries an unknown version");
                    }
                    switchAddress(reader.text(reader.bounded(256)));
                    // The socket is being replaced, so there is nothing left to
                    // read on it; the loop reconnects on its own.
                    return;
                case Protocol.KIND_PREVIEW_CTRL:
                    if (reader.u32() != Protocol.VERSION) {
                        throw new IOException("preview control carries an unknown version");
                    }
                    previewRequested = reader.u8() != 0;
                    break;
                default:
                    break;
            }
            long now = System.currentTimeMillis();
            if (now - lastStats >= STATS_INTERVAL_MS) {
                lastStats = now;
                reportLink(now);
            }
        }
    }

    /**
     * Decides whether the datagram channel is still worth asking the host for.
     *
     * <p>A datagram that never arrives looks exactly like a host with nothing to
     * draw, so the only evidence available is the host's own frame number: if it
     * keeps advancing and the frames here do not, the path is broken and the
     * control socket is asked to take over. The request is repeated until either
     * frames start arriving or the retries run out.
     */
    private void reportLink(long now) {
        boolean markFresh = lastMarkAt > 0 && now - lastMarkAt < 5000;
        boolean behind = markFresh
                && lastMarkSequence - lastDatagramSequence > MARK_LAG_LIMIT
                && now - lastDatagramFrameAt > DATAGRAM_SILENCE_MS;

        if (behind && !datagramSuspended) {
            datagramSuspended = true;
            datagramSuspendedAt = now;
            datagramFailures++;
            datagrams.suspend();
            Log.w(TAG, "datagram channel idle while the host counts "
                    + lastMarkSequence + "; asking for the control socket");
        } else if (datagramSuspended
                && now - datagramSuspendedAt > DATAGRAM_RETRY_MS
                && datagramFailures <= DATAGRAM_RETRIES) {
            // Try again after a cooling-off period. Without this a network that
            // only blocked datagrams for a moment would leave the panel on the
            // slow path until the app was restarted.
            datagramSuspended = false;
            datagramSuspendedAt = now;
            datagrams.resume();
            Log.i(TAG, "retrying the datagram channel");
        }

        byte[] payload = new byte[21];
        writeLe32(payload, 0, Protocol.VERSION);
        putLe64(payload, 4, datagrams.framesAssembled());
        putLe64(payload, 12, datagrams.slicesReceived());
        payload[20] = (byte) (datagramSuspended ? 1 : 0);
        send(Protocol.KIND_LINK_STATS, payload);
    }

    private static void putLe64(byte[] target, int offset, long value) {
        for (int i = 0; i < 8; i++) {
            target[offset + i] = (byte) ((value >>> (8 * i)) & 0xFF);
        }
    }

    /**
     * Applies one frame payload.
     *
     * <p>Returns whether the panel now has something new to draw. Both channels
     * call this, so the sequence check is what keeps the slower one from
     * overwriting the faster one's work.
     */
    private boolean applyFrame(byte[] payload) {
        // A host that predates the datagram body fix wraps the frame in the
        // five-byte message header. That header is self-describing — kind then
        // declared length — so the wrap is recognisable and an old host keeps
        // working instead of driving every frame into the count check.
        if (payload.length > 5 && (payload[0] & 0xFF) == Protocol.KIND_FRAME) {
            int declared = (payload[1] & 0xFF)
                    | ((payload[2] & 0xFF) << 8)
                    | ((payload[3] & 0xFF) << 16)
                    | ((payload[4] & 0xFF) << 24);
            if (declared == payload.length - 5) {
                payload = java.util.Arrays.copyOfRange(payload, 5, payload.length);
            }
        }
        if (payload.length < 20) {
            return false;
        }
        long generation = readLe64(payload, 4);
        long sequence = readLe64(payload, 12);
        synchronized (lock) {
            if (generation == lastGeneration && sequence <= lastSequence) {
                return false;
            }
            lastGeneration = generation;
            lastSequence = sequence;
            try {
                shared.read(new Protocol.Reader(payload, Protocol.KIND_FRAME));
                hasFrame = true;
                cleared = false;
            } catch (IOException error) {
                Log.w(TAG, "frame rejected: " + error.getMessage());
                return false;
            }
        }
        return true;
    }

    private void clearFrame() {
        synchronized (lock) {
            hasFrame = false;
            cleared = true;
        }
    }

    private void readAudioLibrary(Protocol.Reader reader) throws IOException {
        reader.u32();
        int count = reader.bounded(256);
        byte[] payload = reader.payloadCopy();
        // The audio player walks the same payload with explicit offsets: it needs
        // the raw bytes to write each clip, and the reader's cursor belongs to the
        // socket parse rather than to the file copy.
        int offset = Math.min(reader.limit(), 8);
        audio.setLibrary(payload, offset, count);
    }

    // UdpFrameChannel.Sink

    @Override
    public void onFrame(byte[] payload, long channelSequence) {
        lastDatagramFrameAt = System.currentTimeMillis();
        lastDatagramSequence = Math.max(lastDatagramSequence, channelSequence);
        if (applyFrame(payload)) {
            publish("live", true);
        }
    }

    @Override
    public void onClear(long channelSequence) {
        lastDatagramFrameAt = System.currentTimeMillis();
        lastDatagramSequence = Math.max(lastDatagramSequence, channelSequence);
        clearFrame();
    }

    @Override
    public void onRegistered() {
        Log.i(TAG, "host acknowledged this device's datagram channel");
    }

    private static void readHello(Protocol.Reader reader) throws IOException {
        int version = reader.u32();
        reader.u64();
        if (version != Protocol.VERSION) {
            throw new IOException("publisher speaks protocol " + version
                    + ", this build speaks " + Protocol.VERSION);
        }
    }

    private Scene readScene(Protocol.Reader reader) throws IOException {
        reader.u32();
        Scene scene = new Scene();
        scene.generation = reader.u64();
        scene.canvasMinX = reader.f32();
        scene.canvasMaxX = reader.f32();
        scene.canvasMinY = reader.f32();
        scene.canvasMaxY = reader.f32();

        if (reader.u8() != 0) {
            int width = reader.u32();
            int height = reader.u32();
            int size = reader.u32();
            scene.background = decode(reader, size, width, height, "background");
        }

        int textures = reader.bounded(Scene.MAX_TEXTURES);
        scene.textureIds = new int[textures];
        scene.textureImages = new Bitmap[textures];
        for (int i = 0; i < textures; i++) {
            scene.textureIds[i] = reader.u32();
            int width = reader.u32();
            int height = reader.u32();
            int size = reader.u32();
            scene.textureImages[i] = decode(reader, size, width, height, "texture " + i);
        }

        int keys = reader.bounded(Scene.MAX_KEYS);
        scene.keyIds = new int[keys];
        scene.keySides = new int[keys];
        scene.keyBounds = new float[keys * 4];
        scene.keyImages = new Bitmap[keys];
        for (int i = 0; i < keys; i++) {
            scene.keyIds[i] = reader.u32();
            scene.keySides[i] = reader.u8();
            int width = reader.u32();
            int height = reader.u32();
            scene.keyBounds[i * 4] = reader.f32();
            scene.keyBounds[i * 4 + 1] = reader.f32();
            scene.keyBounds[i * 4 + 2] = reader.f32();
            scene.keyBounds[i * 4 + 3] = reader.f32();
            int size = reader.u32();
            scene.keyImages[i] = decode(reader, size, width, height, "key " + i);
        }

        return scene;
    }

    private static Bitmap decode(
            Protocol.Reader reader, int size, int width, int height, String what)
            throws IOException {
        BitmapFactory.Options options = new BitmapFactory.Options();
        options.inPreferredConfig = Bitmap.Config.ARGB_8888;
        options.inScaled = false;
        Bitmap bitmap = BitmapFactory.decodeByteArray(reader.buffer(), reader.position(), size, options);
        reader.advance(size);
        if (bitmap == null) {
            throw new IOException("could not decode " + what);
        }
        if (bitmap.getWidth() != width || bitmap.getHeight() != height) {
            int actualWidth = bitmap.getWidth();
            int actualHeight = bitmap.getHeight();
            bitmap.recycle();
            throw new IOException(what + " is " + actualWidth + "x" + actualHeight
                    + ", expected " + width + "x" + height);
        }
        return bitmap;
    }

    /** The datagram port that belongs to a control port. */
    private static int datagramPort(int controlPort) {
        return controlPort >= 65535 ? controlPort : controlPort + 1;
    }

    private static long readLe64(byte[] source, int offset) {
        long low = readLe32(source, offset) & 0xFFFFFFFFL;
        long high = readLe32(source, offset + 4) & 0xFFFFFFFFL;
        return (high << 32) | low;
    }

    private static int readLe32(byte[] source, int offset) {
        return (source[offset] & 0xFF)
                | ((source[offset + 1] & 0xFF) << 8)
                | ((source[offset + 2] & 0xFF) << 16)
                | ((source[offset + 3] & 0xFF) << 24);
    }

    /** Splits {@code host:port} (or bare {@code host}) into an endpoint. */
    private static InetSocketAddress parseAddress(String address) {
        int colon = address.lastIndexOf(':');
        if (colon <= 0) {
            return new InetSocketAddress(address, DEFAULT_PORT);
        }
        int port;
        try {
            port = Integer.parseInt(address.substring(colon + 1));
        } catch (NumberFormatException ignored) {
            return new InetSocketAddress(address, DEFAULT_PORT);
        }
        return new InetSocketAddress(address.substring(0, colon), port);
    }

    private void publish(String text, boolean live) {
        Listener sink = listener;
        if (sink == null) {
            return;
        }
        // Every accepted frame reports itself live, which is once per frame at
        // sixty hertz. Repeating the state that is already on the panel changes
        // nothing on it, so only a change is worth carrying across.
        synchronized (publishLock) {
            if (text.equals(lastPublished) && live == lastPublishedLive) {
                return;
            }
            lastPublished = text;
            lastPublishedLive = live;
        }
        sink.onStatus(text, live);
    }

    private static void closeQuietly(Socket target) {
        if (target == null) {
            return;
        }
        try {
            target.close();
        } catch (IOException ignored) {
            // Nothing useful follows a failed close on a socket we are discarding.
        }
    }

    private static void sleep(long millis) {
        try {
            Thread.sleep(millis);
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
        }
    }
}
