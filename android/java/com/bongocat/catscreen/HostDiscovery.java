package com.bongocat.catscreen;

import android.util.Log;

import java.io.IOException;
import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.InterfaceAddress;
import java.net.NetworkInterface;
import java.net.SocketTimeoutException;
import java.util.Collections;

/**
 * Finds the host on the local network without being told where it is.
 *
 * <p>A panel remembers one address, and that address is whatever it was last
 * told. That is enough while the path it names still works and useless the
 * moment it does not: a panel left pointing at a loopback port cannot reach a
 * host that has moved to the network, and the host cannot correct it, because
 * the correction would have to travel over the path that is down.
 *
 * <p>The datagram side already answers an announcement with an acknowledgement.
 * Announcing to the broadcast address asks every host on the network at once,
 * and the source of the first answer is an address that works. The answer
 * arrives on the datagram port, which is always the control port plus one, so
 * the port to dial is the sender's port minus one.
 */
final class HostDiscovery {

    private static final String TAG = "CatScreen";

    /** How long one announcement waits for answers. */
    private static final int LISTEN_TIMEOUT_MS = 400;

    private HostDiscovery() {}

    /**
     * Asks the network for a host and returns the first one that answers.
     *
     * @param controlPort the port the control socket listens on, from the
     *     address the panel is currently trying
     * @param timeoutMs how long to keep asking
     * @return the control address of a host, or null when nobody answered
     */
    static InetSocketAddress find(int controlPort, int timeoutMs) {
        if (controlPort <= 0) {
            return null;
        }
        DatagramSocket socket = null;
        try {
            socket = new DatagramSocket();
            socket.setBroadcast(true);
            socket.setSoTimeout(LISTEN_TIMEOUT_MS);
            byte[] request = Protocol.datagramRegister(System.nanoTime());
            // Both addresses a phone-style stack may need: the limited broadcast
            // reaches every host on the link, and some access points drop it
            // while forwarding the subnet broadcast.
            broadcast(socket, request, controlPort, "255.255.255.255");
            broadcast(socket, request, controlPort, subnetBroadcast());
            long deadline = System.currentTimeMillis() + timeoutMs;
            byte[] buffer = new byte[64];
            while (System.currentTimeMillis() < deadline) {
                DatagramPacket answer = new DatagramPacket(buffer, buffer.length);
                try {
                    socket.receive(answer);
                } catch (SocketTimeoutException idle) {
                    continue;
                }
                InetSocketAddress host = hostFrom(answer);
                if (host != null) {
                    Log.i(TAG, "discovered host at " + host.getAddress().getHostAddress()
                            + ":" + host.getPort());
                    return host;
                }
            }
        } catch (IOException error) {
            Log.w(TAG, "discovery failed: " + error.getMessage());
        } finally {
            if (socket != null) {
                socket.close();
            }
        }
        return null;
    }

    private static void broadcast(DatagramSocket socket, byte[] request, int controlPort,
            String address) {
        if (address == null) {
            return;
        }
        try {
            InetAddress target = InetAddress.getByName(address);
            socket.send(new DatagramPacket(request, request.length, target, controlPort + 1));
        } catch (IOException ignored) {
            // An address this stack cannot resolve or route is simply one fewer
            // place to ask; the other announcement still goes out.
        }
    }

    /**
     * The broadcast address of the interface that carries the link.
     *
     * <p>Derived from the panel's own address and prefix length rather than
     * assumed to be `x.x.x.255`: on a subnet wider than `/24` that guess misses
     * every host outside the last octet, and on a narrower one it sprays the
     * neighbourhood.
     */
    private static String subnetBroadcast() {
        try {
            for (NetworkInterface nif : Collections.list(NetworkInterface.getNetworkInterfaces())) {
                if (nif == null || !nif.isUp() || nif.isLoopback()) {
                    continue;
                }
                for (InterfaceAddress ifa : nif.getInterfaceAddresses()) {
                    InetAddress address = ifa.getAddress();
                    if (address == null || address.getAddress().length != 4 || address.isLoopbackAddress()) {
                        continue;
                    }
                    InetAddress broadcast = ifa.getBroadcast();
                    if (broadcast != null) {
                        return broadcast.getHostAddress();
                    }
                    String derived = fromPrefix(address, ifa.getNetworkPrefixLength());
                    if (derived != null) {
                        return derived;
                    }
                }
            }
        } catch (Exception ignored) {
            // No usable interface: the limited broadcast is the only ask left.
        }
        return null;
    }

    private static String fromPrefix(InetAddress address, int prefix) {
        if (prefix < 0 || prefix > 32) {
            return null;
        }
        byte[] bytes = address.getAddress();
        int value = ((bytes[0] & 0xFF) << 24) | ((bytes[1] & 0xFF) << 16)
                | ((bytes[2] & 0xFF) << 8) | (bytes[3] & 0xFF);
        int mask = prefix == 0 ? 0 : -1 << (32 - prefix);
        int broadcast = value | ~mask;
        return String.format("%d.%d.%d.%d",
                (broadcast >>> 24) & 0xFF, (broadcast >>> 16) & 0xFF,
                (broadcast >>> 8) & 0xFF, broadcast & 0xFF);
    }

    /** The control address an acknowledgement came from, or null for anything else. */
    private static InetSocketAddress hostFrom(DatagramPacket answer) {
        int length = answer.getLength();
        if (length < Protocol.DATAGRAM_HEADER_LEN) {
            return null;
        }
        byte[] data = answer.getData();
        for (int i = 0; i < Protocol.DATAGRAM_MAGIC.length; i++) {
            if (data[i] != Protocol.DATAGRAM_MAGIC[i]) {
                return null;
            }
        }
        if ((data[4] & 0xFF) != Protocol.DATAGRAM_REGISTER_ACK
                || (data[5] & 0xFF) != Protocol.VERSION) {
            return null;
        }
        InetAddress from = answer.getAddress();
        int port = answer.getPort() - 1;
        if (from == null || from.isLoopbackAddress() || port <= 0) {
            return null;
        }
        return new InetSocketAddress(from, port);
    }
}
