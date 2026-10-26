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
    private static final int PORT = 7391;
    private static final int CONNECT_TIMEOUT_MS = 2000;
    private static final int RETRY_DELAY_MS = 750;

    private final Object lock = new Object();
    private final Frame shared = new Frame();

    private Scene pendingScene;
    private boolean hasFrame;
    private boolean cleared;

    private volatile boolean running = true;
    private volatile Listener listener;
    private volatile Socket socket;

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
                connection.connect(new InetSocketAddress("127.0.0.1", PORT), CONNECT_TIMEOUT_MS);
                connection.setTcpNoDelay(true);
                socket = connection;
                publish("connected", false);
                read(connection);
                publish("publisher closed the connection", false);
            } catch (IOException error) {
                if (running) {
                    publish("waiting for the publisher", false);
                    Log.w(TAG, "connection lost: " + error.getMessage());
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

        if (reader.u8() != 0) {
            int width = reader.u32();
            int height = reader.u32();
            scene.backgroundMinX = reader.f32();
            scene.backgroundMaxX = reader.f32();
            scene.backgroundMinY = reader.f32();
            scene.backgroundMaxY = reader.f32();
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
