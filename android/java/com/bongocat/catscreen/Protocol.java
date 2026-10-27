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
