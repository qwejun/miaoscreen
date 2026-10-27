package com.bongocat.catscreen;

import android.util.Log;

import java.io.IOException;
import java.net.DatagramPacket;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.DatagramSocket;
import java.util.Arrays;

/**
 * Frames over UDP.
 *
 * <p>The control socket stays exactly as it was: textures, the strip, the audio
 * library and every command travel over TCP, where a lost byte is resent. Only
 * the frame stream moves here. A frame is a complete state — the next one
 * replaces it — so a frame that arrives late is worth nothing, and over Wi-Fi a
 * TCP send buffer full of late frames is the difference between a cat that
 * responds and a cat that keeps acting out what the keyboard did two seconds ago.
 *
 * <p>One frame is cut into datagrams below the path MTU, because a frame split by
 * a router is a frame that disappears whole when any fragment is lost. The
 * assembly here is deliberately lossy on the same principle: a frame that is
 * missing a slice is dropped the moment the next one starts arriving.
 */
final class UdpFrameChannel {

    interface Sink {
        /** A complete frame payload, exactly as it would have looked on the wire. */
        void onFrame(byte[] payload, long channelSequence);

        /** The host erased the panel. */
        void onClear(long channelSequence);

        /** The host answered an announcement, so it knows where to send. */
        void onRegistered();
    }

    private static final String TAG = "CatScreen";

    /** How often the host is told where this device is. */
    private static final long ANNOUNCE_INTERVAL_MS = 1000;

    /** How long a receive blocks before the thread looks at the world again. */
    private static final int RECEIVE_TIMEOUT_MS = 300;

    /**
     * The largest frame this device will assemble.
     *
     * <p>Frames are ten kilobytes; a megabyte is two orders of magnitude of room
     * for a model with more meshes than this panel will ever be sent, and the cap
     * is what stops a malformed length from asking for an allocation.
     */
    private static final int MAX_FRAME_BYTES = 1 << 20;

    private final Object lock = new Object();
    private final Sink sink;

    private volatile boolean running = true;
    private DatagramSocket socket;
    private InetSocketAddress host;
    private long nonce;

    /** Datagrams that arrived, complete frames or not. Read by the stats report. */
    private long slicesReceived;
    private long framesAssembled;
    private volatile boolean registered;

    /**
     * Set while the host is being asked to use the control socket again.
     *
     * <p>While it is set nothing is announced: re-announcing would flip the host
     * straight back to datagrams, which is the oscillation this flag exists to
     * avoid. The publisher clears it after a cooling-off period to try again.
     */
    private volatile boolean suspended;

    private long assemblingSequence = Long.MIN_VALUE;
    private byte[] assembling;
    private boolean[] assemblingSlices;
    private int assemblingCount;
    private int assemblingReceived;
    private int assemblingTotal;

    UdpFrameChannel(Sink sink) {
        this.sink = sink;
    }

    /** Points the channel at a host, restarting it if the address changed. */
    void open(String address, int port) {
        InetSocketAddress parsed = new InetSocketAddress(address, port);
        synchronized (lock) {
            if (parsed.equals(host) && socket != null && !socket.isClosed()) {
                return;
            }
            closeSocket();
            host = parsed;
            nonce = System.nanoTime();
            socket = null;
            registered = false;
            assemblingSequence = Long.MIN_VALUE;
            try {
                DatagramSocket created = new DatagramSocket();
                created.setSoTimeout(RECEIVE_TIMEOUT_MS);
                created.setBroadcast(false);
                socket = created;
            } catch (IOException error) {
                Log.w(TAG, "datagram channel unavailable: " + error.getMessage());
                return;
            }
            Thread thread = new Thread(new Runnable() {
                @Override
                public void run() {
                    receiveLoop();
                }
            }, "udp-frame-channel");
            thread.setDaemon(true);
            thread.start();
        }
    }

    /** Stops announcing for {@code millis}; the publisher decides when to retry. */
    void suspend() {
        suspended = true;
        registered = false;
    }

    void resume() {
        suspended = false;
    }

    long slicesReceived() {
        return slicesReceived;
    }

    long framesAssembled() {
        return framesAssembled;
    }

    boolean isRegistered() {
        return registered;
    }

    void stop() {
        running = false;
        synchronized (lock) {
            closeSocket();
        }
    }

    private void closeSocket() {
        DatagramSocket current = socket;
        socket = null;
        if (current != null) {
            current.close();
        }
    }

    private void receiveLoop() {
        byte[] buffer = new byte[2048];
        DatagramPacket packet = new DatagramPacket(buffer, buffer.length);
        long lastAnnounce = 0;
        while (running) {
            DatagramSocket current;
            InetSocketAddress target;
            synchronized (lock) {
                current = socket;
                target = host;
            }
            if (current == null || target == null) {
                return;
            }
            long now = System.currentTimeMillis();
            if (!suspended && now - lastAnnounce >= ANNOUNCE_INTERVAL_MS) {
                lastAnnounce = now;
                announce(current, target);
            }
            try {
                packet.setLength(buffer.length);
                current.receive(packet);
                handle(current, buffer, packet.getLength());
            } catch (java.net.SocketTimeoutException idle) {
                // Nothing arrived. The announcement above is what keeps the
                // return path open, so an idle second costs one small packet.
            } catch (IOException error) {
                if (running && socket != null) {
                    Log.w(TAG, "datagram receive failed: " + error.getMessage());
                    return;
                }
                return;
            }
        }
    }

    private void announce(DatagramSocket current, InetSocketAddress target) {
        byte[] packet = new byte[Protocol.DATAGRAM_HEADER_LEN + 8];
        System.arraycopy(Protocol.DATAGRAM_MAGIC, 0, packet, 0, 4);
        packet[4] = (byte) Protocol.DATAGRAM_REGISTER;
        packet[5] = (byte) Protocol.VERSION;
        putLe64(packet, 8, nonce);
        try {
            current.send(new DatagramPacket(packet, packet.length, target));
        } catch (IOException ignored) {
            // The host may not be listening yet; the next announcement retries.
        }
    }

    private void handle(DatagramSocket current, byte[] buffer, int length) {
        if (length < Protocol.DATAGRAM_HEADER_LEN) {
            return;
        }
        for (int i = 0; i < 4; i++) {
            if (buffer[i] != Protocol.DATAGRAM_MAGIC[i]) {
                return;
            }
        }
        if ((buffer[5] & 0xFF) != Protocol.VERSION) {
            return;
        }
        int type = buffer[4] & 0xFF;
        switch (type) {
            case Protocol.DATAGRAM_REGISTER_ACK:
                if (length >= Protocol.DATAGRAM_HEADER_LEN + 8
                        && readLe64(buffer, 8) == nonce
                        && !registered) {
                    registered = true;
                    sink.onRegistered();
                    Log.i(TAG, "datagram channel up");
                }
                break;
            case Protocol.DATAGRAM_FRAME_SLICE:
                slicesReceived++;
                readSlice(buffer, length);
                break;
            case Protocol.DATAGRAM_CLEAR:
                if (length >= Protocol.DATAGRAM_HEADER_LEN + 8) {
                    long sequence = readLe64(buffer, 8);
                    assemblingSequence = Long.MIN_VALUE;
                    sink.onClear(sequence);
                }
                break;
            default:
                break;
        }
    }

    /**
     * Collects one frame's slices.
     *
     * <p>A slice from a different channel sequence than the one being assembled
     * starts over: the previous frame is already superseded, and waiting for its
     * missing slice would mean holding a picture nobody will ever see.
     */
    private void readSlice(byte[] buffer, int length) {
        if (length < Protocol.DATAGRAM_HEADER_LEN + 16) {
            return;
        }
        long sequence = readLe64(buffer, 8);
        int total = readLe32(buffer, 16);
        int index = readLe16(buffer, 20);
        int count = readLe16(buffer, 22);
        if (total <= 0 || total > MAX_FRAME_BYTES || count <= 0 || index < 0 || index >= count) {
            return;
        }
        if (sequence != assemblingSequence) {
            assemblingSequence = sequence;
            assembling = new byte[total];
            assemblingSlices = new boolean[count];
            assemblingCount = count;
            assemblingReceived = 0;
            assemblingTotal = total;
        } else if (assembling == null || assemblingSlices.length != count || assemblingTotal != total) {
            return;
        }
        if (assemblingSlices[index]) {
            return;
        }
        // Every slice but the last is full, so the offset is the index times the
        // payload size the host used; the total only bounds the last one.
        int offset = index * Protocol.DATAGRAM_PAYLOAD;
        int body = length - (Protocol.DATAGRAM_HEADER_LEN + 16);
        if (offset < 0 || offset + body > assembling.length) {
            return;
        }
        System.arraycopy(buffer, Protocol.DATAGRAM_HEADER_LEN + 16, assembling, offset, body);
        assemblingSlices[index] = true;
        assemblingReceived++;
        if (assemblingReceived == count) {
            byte[] payload = Arrays.copyOf(assembling, assemblingTotal);
            assembling = null;
            assemblingSlices = null;
            assemblingSequence = Long.MIN_VALUE;
            framesAssembled++;
            sink.onFrame(payload, sequence);
        }
    }

    private static int readLe16(byte[] source, int offset) {
        return (source[offset] & 0xFF) | ((source[offset + 1] & 0xFF) << 8);
    }

    private static int readLe32(byte[] source, int offset) {
        return (source[offset] & 0xFF)
                | ((source[offset + 1] & 0xFF) << 8)
                | ((source[offset + 2] & 0xFF) << 16)
                | ((source[offset + 3] & 0xFF) << 24);
    }

    private static long readLe64(byte[] source, int offset) {
        long low = readLe32(source, offset) & 0xFFFFFFFFL;
        long high = readLe32(source, offset + 4) & 0xFFFFFFFFL;
        return (high << 32) | low;
    }

    private static void putLe64(byte[] target, int offset, long value) {
        target[offset] = (byte) (value & 0xFF);
        target[offset + 1] = (byte) ((value >>> 8) & 0xFF);
        target[offset + 2] = (byte) ((value >>> 16) & 0xFF);
        target[offset + 3] = (byte) ((value >>> 24) & 0xFF);
        target[offset + 4] = (byte) ((value >>> 32) & 0xFF);
        target[offset + 5] = (byte) ((value >>> 40) & 0xFF);
        target[offset + 6] = (byte) ((value >>> 48) & 0xFF);
        target[offset + 7] = (byte) ((value >>> 56) & 0xFF);
    }
}
