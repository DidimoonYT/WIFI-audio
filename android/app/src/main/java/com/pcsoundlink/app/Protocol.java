package com.pcsoundlink.app;

import org.json.JSONException;
import org.json.JSONObject;

import java.net.InetAddress;
import java.net.InterfaceAddress;
import java.net.NetworkInterface;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Set;

/**
 * Wire protocol shared with pc-server/server.py.
 *
 * Control messages are JSON over UDP (port 47832). Audio is raw PCM16 stereo
 * split into MTU-safe chunks; each datagram starts with a 16 byte header.
 */
final class Protocol {
    static final int VERSION = 1;

    static final int CONTROL_PORT = 47832;
    static final int DISCOVERY_PORT = 47833;

    static final int SAMPLE_RATE = 48000;
    static final int CHANNELS = 2;
    static final int FRAME_MS = 10;
    static final int BYTES_PER_FRAME = SAMPLE_RATE / 1000 * FRAME_MS * CHANNELS * 2; // 3840
    static final int CHUNK_BYTES = 960;
    static final int HEADER_BYTES = 16;
    static final int MAX_FRAME_BYTES = 64 * 1024;

    static final String MAGIC_DISCOVER = "PCSOUND_DISCOVER";
    static final String MAGIC_SERVER = "PCSOUND_SERVER";
    static final String MAGIC_CTRL = "PCSOUND_CTRL";

    private Protocol() {
    }

    static JSONObject discover() throws JSONException {
        return new JSONObject()
                .put("magic", MAGIC_DISCOVER)
                .put("v", VERSION);
    }

    static JSONObject control(String cmd) throws JSONException {
        return new JSONObject()
                .put("magic", MAGIC_CTRL)
                .put("v", VERSION)
                .put("cmd", cmd);
    }

    static byte[] packetHeader(int seq, int timestampMs, int frameBytes,
                               int chunkIndex, int chunkCount) {
        ByteBuffer bb = ByteBuffer.allocate(HEADER_BYTES).order(ByteOrder.BIG_ENDIAN);
        bb.put((byte) 'P').put((byte) 'S');
        bb.put((byte) VERSION);
        bb.put((byte) 0);
        bb.putInt(seq);
        bb.putInt(timestampMs);
        bb.putShort((short) frameBytes);
        bb.put((byte) chunkIndex);
        bb.put((byte) chunkCount);
        return bb.array();
    }

    static Header parseHeader(byte[] data, int offset, int length) {
        if (length < HEADER_BYTES) {
            return null;
        }
        ByteBuffer bb = ByteBuffer.wrap(data, offset, HEADER_BYTES).order(ByteOrder.BIG_ENDIAN);
        if (bb.get() != 'P' || bb.get() != 'S') {
            return null;
        }
        Header h = new Header();
        h.version = bb.get() & 0xff;
        h.flags = bb.get() & 0xff;
        h.seq = bb.getInt();
        h.timestampMs = bb.getInt();
        h.frameBytes = bb.getShort() & 0xffff;
        h.chunkIndex = bb.get() & 0xff;
        h.chunkCount = bb.get() & 0xff;
        if (h.version != VERSION || h.frameBytes <= 0 || h.frameBytes > MAX_FRAME_BYTES) {
            return null;
        }
        if (h.chunkCount <= 0 || h.chunkIndex >= h.chunkCount) {
            return null;
        }
        return h;
    }

    static byte[] utf8(JSONObject obj) {
        return obj.toString().getBytes(StandardCharsets.UTF_8);
    }

    /**
     * Every broadcast address the phone can currently see. Sending to all of
     * them (instead of only 255.255.255.255) makes discovery work on routers
     * and VPN setups that do not route the global broadcast.
     */
    static List<InetAddress> broadcastAddresses() {
        Set<InetAddress> result = new LinkedHashSet<>();
        try {
            result.add(InetAddress.getByName("255.255.255.255"));
        } catch (Exception ignored) {
        }
        try {
            for (NetworkInterface ni : Collections.list(NetworkInterface.getNetworkInterfaces())) {
                if (!ni.isUp() || ni.isLoopback()) {
                    continue;
                }
                for (InterfaceAddress ia : ni.getInterfaceAddresses()) {
                    InetAddress b = ia.getBroadcast();
                    if (b != null) {
                        result.add(b);
                    }
                }
            }
        } catch (Exception ignored) {
        }
        return new ArrayList<>(result);
    }

    static final class Header {
        int version;
        int flags;
        int seq;
        int timestampMs;
        int frameBytes;
        int chunkIndex;
        int chunkCount;
    }
}
