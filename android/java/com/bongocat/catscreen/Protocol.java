package com.bongocat.catscreen;

import java.io.EOFException;
import java.io.IOException;
import java.io.InputStream;

/**
 * The wire format, mirrored from the publisher's {@code protocol.rs}.
 *
 * <p>Little-endian throughout. Every message is {@code kind: u8}, {@code length:
 * u32} and a payload, so a client can skip a message it does not understand
 * instead of losing the stream. The payload begins with the protocol version.
 */
final class Protocol {

    static final int KIND_HELLO = 0x10;
    static final int KIND_RESOURCES = 0x11;
    static final int KIND_FRAME = 0x12;
    static final int KIND_CLEAR = 0x13;
    static final int KIND_OVERLAY = 0x14;
    /** Host asks for (or stops asking for) snapshots of what the panel shows. */
    static final int KIND_PREVIEW_CTRL = 0x15;
    /** One snapshot, device to host: a JPEG of this panel's framebuffer. */
    static final int KIND_PREVIEW_FRAME = 0x16;
    /** The sounds the model can make, sent once per model generation. */
    static final int KIND_AUDIO_LIBRARY = 0x17;
    /** Play one clip from the library: `id`, `volume`. */
    static final int KIND_AUDIO_PLAY = 0x18;
    /** Stop whatever is playing. */
    static final int KIND_AUDIO_STOP = 0x19;
    /** Device to host: how many datagrams arrived, and whether to stop sending. */
    static final int KIND_LINK_STATS = 0x1A;
    /** Host to device: one frame number that was put on the datagram channel. */
    static final int KIND_FRAME_MARK = 0x1B;

    /** The four bytes every datagram begins with. */
    static final byte[] DATAGRAM_MAGIC = {'M', 'C', 'A', 'T'};
    /** `MAGIC` + `type: u8` + `version: u8` + `reserved: u16`. */
    static final int DATAGRAM_HEADER_LEN = 8;
    static final int DATAGRAM_REGISTER = 1;
    static final int DATAGRAM_REGISTER_ACK = 2;
    static final int DATAGRAM_FRAME_SLICE = 3;
    static final int DATAGRAM_CLEAR = 4;
    /** Frame payload bytes per datagram. Must match the publisher's constant. */
    static final int DATAGRAM_PAYLOAD = 1180;

    static final int VERSION = 1;

    static final int BLEND_NORMAL = 0;
    static final int BLEND_ADDITIVE = 1;
    static final int BLEND_MULTIPLICATIVE = 2;

    static final int FLAG_VISIBLE = 1;
    static final int FLAG_DOUBLE_SIDED = 1 << 1;
    static final int FLAG_INVERTED_MASK = 1 << 2;
    static final int FLAG_BLEND_SHIFT = 3;
    static final int FLAG_BLEND_MASK = 0b11 << FLAG_BLEND_SHIFT;

    static final int SIDE_LEFT = 0;
    static final int SIDE_RIGHT = 1;

    private Protocol() {
    }

    /** Reads whole messages off the socket into one reusable payload buffer. */
    static final class Reader {

        private final InputStream in;
        private final byte[] header = new byte[5];
        private byte[] payload = new byte[16 * 1024];

        private int position;
        private int limit;

        int kind;

        Reader(InputStream in) {
            this.in = in;
        }

        /**
         * A reader over a body that has already been separated from its header.
         *
         * <p>Frames arrive as datagrams, where the {@code kind} and {@code length}
         * were consumed by the assembler rather than by a socket read. The body is
         * exactly what the control socket would have delivered after the five
         * header bytes.
         */
        Reader(byte[] body, int kind) {
            this.in = null;
            this.payload = body;
            this.position = 0;
            this.limit = body.length;
            this.kind = kind;
        }

        /** Blocks for the next message; returns false only on a clean close. */
        boolean next() throws IOException {
            if (!readFully(header, 0, header.length)) {
                return false;
            }
            int length = (header[1] & 0xFF)
                    | ((header[2] & 0xFF) << 8)
                    | ((header[3] & 0xFF) << 16)
                    | ((header[4] & 0xFF) << 24);
            if (length < 0) {
                throw new IOException("negative message length");
            }
            if (payload.length < length) {
                payload = new byte[Math.max(length, payload.length * 2)];
            }
            if (length > 0 && !readFully(payload, 0, length)) {
                throw new EOFException("message truncated");
            }
            kind = header[0] & 0xFF;
            position = 0;
            limit = length;
            return true;
        }

        private boolean readFully(byte[] target, int offset, int count) throws IOException {
            int read = 0;
            while (read < count) {
                int n = in.read(target, offset + read, count - read);
                if (n < 0) {
                    // End of stream. Returning true with zero bytes read is how
                    // this client used to replay its last message forever: the
                    // header buffer still held the previous one, so a closed
                    // socket looked like an endless run of identical frames and
                    // the reader never reconnected. A clean close is only clean
                    // on a message boundary; anything else is a torn message.
                    if (read == 0) {
                        return false;
                    }
                    throw new EOFException("message truncated");
                }
                if (n == 0) {
                    // A stream that is open but delivers nothing must not turn
                    // this into a busy loop: the panel would freeze while the
                    // reader burned a whole core, which is exactly how the first
                    // version of this client failed.
                    throw new IOException("stream stalled");
                }
                read += n;
            }
            return true;
        }

        /** The whole payload; image decoders read straight out of it. */
        byte[] buffer() {
            return payload;
        }

        /** A copy of the payload that survives the next message. */
        byte[] payloadCopy() {
            byte[] copy = new byte[limit];
            System.arraycopy(payload, 0, copy, 0, limit);
            return copy;
        }

        int limit() {
            return limit;
        }

        int position() {
            return position;
        }

        void advance(int count) throws IOException {
            if (count < 0 || position + count > limit) {
                throw new IOException("message overrun");
            }
            position += count;
        }

        int u8() throws IOException {
            require(1);
            return payload[position++] & 0xFF;
        }

        int u16() throws IOException {
            require(2);
            int value = (payload[position] & 0xFF) | ((payload[position + 1] & 0xFF) << 8);
            position += 2;
            return value;
        }

        int u32() throws IOException {
            require(4);
            int value = (payload[position] & 0xFF)
                    | ((payload[position + 1] & 0xFF) << 8)
                    | ((payload[position + 2] & 0xFF) << 16)
                    | ((payload[position + 3] & 0xFF) << 24);
            position += 4;
            return value;
        }

        int i32() throws IOException {
            return u32();
        }

        long u64() throws IOException {
            long low = u32() & 0xFFFFFFFFL;
            long high = u32() & 0xFFFFFFFFL;
            return (high << 32) | low;
        }

        float f32() throws IOException {
            return Float.intBitsToFloat(u32());
        }

        /** A count the following data must fit in; rejects a corrupt stream. */
        int bounded(int maximum) throws IOException {
            int value = u32();
            if (value < 0 || value > maximum) {
                throw new IOException("implausible count " + value);
            }
            return value;
        }

        /** The same check for a `u16` count. */
        int bounded16(int maximum) throws IOException {
            int value = u16();
            if (value > maximum) {
                throw new IOException("implausible count " + value);
            }
            return value;
        }

        private void require(int bytes) throws IOException {
            if (position + bytes > limit) {
                throw new IOException("message underrun");
            }
        }
    }
}
