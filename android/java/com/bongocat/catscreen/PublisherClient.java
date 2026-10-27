package com.bongocat.catscreen;

import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.util.Log;

import java.io.BufferedInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.net.InetSocketAddress;
import java.net.Socket;

/**
 * Keeps one TCP connection to the publisher alive and hands finished frames to
 * the render thread.
 *
 * <p>The device reaches the publisher through {@code adb reverse tcp:7391
 * tcp:7391}, so the address is the device's own loopback. A dropped connection is
 * retried forever: the publisher is an ordinary desktop program and the USB cable
 * is allowed to be unplugged.
 *
 * <p>Parsing happens here, never on the render thread. The two are decoupled by a
 * single copied frame: the render thread is allowed to be slower than the stream
 * and simply misses frames, which is what keeps the picture in step with the
 * newest state rather than falling behind by seconds.
 */
final class PublisherClient implements Runnable {

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
     * The publisher sends frames continuously, so a socket that stays silent is a
     * socket whose tunnel is gone. Without this the reader blocks in
     * {@code read()} forever after the adb server is restarted: the device's end
     * of the reverse tunnel is never closed, so no end-of-stream arrives and the
     * cat freezes while the thread sits there.
     */
    private static final int READ_TIMEOUT_MS = 4000;

    private final Object lock = new Object();
    private final Frame shared = new Frame();

    /** Where the publisher lives. The loopback default is the reverse tunnel;
     * a LAN address covers the case where adb's tunnel is unavailable. */
    private final String address;

    PublisherClient() {
        this(DEFAULT_ADDRESS);
    }

    PublisherClient(String address) {
        this.address = address != null ? address : DEFAULT_ADDRESS;
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

    void setListener(Listener listener) {
        this.listener = listener;
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

    /**
     * Sends one snapshot to the host.
     *
     * <p>Called from the render thread at a low rate, so a blocking write is
     * acceptable: the socket's own send timeout bounds it, and the frame stream in
     * the other direction is unaffected. A failed write is dropped silently; the
     * next snapshot will try again, and the reader will notice a dead link first.
     */
    void sendPreview(byte[] jpeg) {
        Socket current = socket;
        if (current == null || jpeg == null || jpeg.length == 0) {
            return;
        }
        synchronized (sendLock) {
            try {
                java.io.OutputStream out = current.getOutputStream();
                byte[] header = new byte[5];
                header[0] = (byte) Protocol.KIND_PREVIEW_FRAME;
                int length = 8 + jpeg.length;
                header[1] = (byte) (length & 0xFF);
                header[2] = (byte) ((length >>> 8) & 0xFF);
                header[3] = (byte) ((length >>> 16) & 0xFF);
                header[4] = (byte) ((length >>> 24) & 0xFF);
                out.write(header);
                byte[] head = new byte[8];
                writeLe32(head, 0, Protocol.VERSION);
                writeLe32(head, 4, jpeg.length);
                out.write(head);
                out.write(jpeg);
                out.flush();
            } catch (IOException error) {
                Log.w(TAG, "snapshot dropped: " + error.getMessage());
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
        closeQuietly(socket);
        socket = null;
    }

    @Override
    public void run() {
        while (running) {
            Socket connection = new Socket();
            try {
                connection.connect(parseAddress(address), CONNECT_TIMEOUT_MS);
                connection.setTcpNoDelay(true);
                connection.setSoTimeout(READ_TIMEOUT_MS);
                socket = connection;
                publish("connected", false);
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
            sleep(RETRY_DELAY_MS);
        }
    }

    private void read(Socket connection) throws IOException {
        InputStream raw = new BufferedInputStream(connection.getInputStream(), 64 * 1024);
        Protocol.Reader reader = new Protocol.Reader(raw);
        boolean live = false;

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
                    synchronized (lock) {
                        shared.read(reader);
                        hasFrame = true;
                        cleared = false;
                    }
                    if (!live) {
                        live = true;
                        publish("live", true);
                    }
                    break;
                case Protocol.KIND_CLEAR:
                    if (reader.u32() != Protocol.VERSION) {
                        throw new IOException("clear carries an unknown version");
                    }
                    synchronized (lock) {
                        hasFrame = false;
                        cleared = true;
                    }
                    break;
                case Protocol.KIND_OVERLAY:
                    Overlay overlay = Overlay.read(reader);
                    synchronized (lock) {
                        pendingOverlay = overlay;
                    }
                    break;
                case Protocol.KIND_PREVIEW_CTRL:
                    if (reader.u32() != Protocol.VERSION) {
                        throw new IOException("preview control carries an unknown version");
                    }
                    previewRequested = reader.u8() != 0;
                    break;
                default:
                    break;
            }
        }
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
        if (sink != null) {
            sink.onStatus(text, live);
        }
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
