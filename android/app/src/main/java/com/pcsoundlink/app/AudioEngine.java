package com.pcsoundlink.app;

import android.content.Context;
import android.content.SharedPreferences;
import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioManager;
import android.media.AudioTrack;
import android.net.wifi.WifiManager;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;
import android.util.Log;

import org.json.JSONObject;

import java.io.IOException;
import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.SocketException;
import java.net.SocketTimeoutException;
import java.nio.charset.StandardCharsets;
import java.text.SimpleDateFormat;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Date;
import java.util.HashMap;
import java.util.Iterator;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.TreeMap;
import java.util.concurrent.CopyOnWriteArrayList;

/**
 * Finds the PC, receives the PCM stream and plays it with a small adaptive
 * jitter buffer. All public methods are safe to call from the UI thread.
 */
public final class AudioEngine {
    private static final String TAG = "PcSoundLink";
    private static final String PREFS = "pcsound";
    private static final int SERVER_TIMEOUT_MS = 7000;
    private static final int STREAM_TIMEOUT_MS = 3500;
    private static final int CONNECT_TIMEOUT_MS = 6000;
    private static final int ADJUST_INTERVAL_MS = 500;

    public enum State {IDLE, SEARCHING, CONNECTING, STREAMING, ERROR}

    public interface Listener {
        void onState(State state, String message);

        void onServers(List<ServerInfo> servers);

        void onStats(Stats stats);

        void onMute(boolean muted, boolean supported);

        void onLog(String line);
    }

    public static final class Stats {
        public final boolean streaming;
        public final int bufferMs;
        public final long rttMs;
        public final int lostFrames;
        public final int droppedFrames;
        public final int playedFrames;

        Stats() {
            this(false, 0, -1, 0, 0, 0);
        }

        Stats(boolean streaming, int bufferMs, long rttMs, int lostFrames,
              int droppedFrames, int playedFrames) {
            this.streaming = streaming;
            this.bufferMs = bufferMs;
            this.rttMs = rttMs;
            this.lostFrames = lostFrames;
            this.droppedFrames = droppedFrames;
            this.playedFrames = playedFrames;
        }
    }

    private static volatile AudioEngine instance;

    private final Context appContext;
    private final Handler mainHandler = new Handler(Looper.getMainLooper());
    private final CopyOnWriteArrayList<Listener> listeners = new CopyOnWriteArrayList<>();
    private final SharedPreferences prefs;

    private final Object discoveryLock = new Object();
    private final Object serversLock = new Object();
    private final Object sessionLock = new Object();
    private final Object logLock = new Object();

    private final Map<String, ServerInfo> servers = new LinkedHashMap<>();
    private final ArrayList<String> logLines = new ArrayList<>();
    private String serversSignature = "";

    private volatile boolean discoveryWanted = false;
    private volatile DatagramSocket discoverySocket;
    private volatile Thread discoveryThread;
    private WifiManager.MulticastLock multicastLock;

    private volatile State state = State.IDLE;
    private volatile String stateMessage = "";
    private volatile Session current;
    private volatile int connectGeneration = 0;
    private volatile Stats stats = new Stats();

    private volatile boolean muted = false;
    private volatile boolean muteSupported = true;
    private volatile boolean autoConnectBlocked = false;
    private volatile int latencyMs = 60;
    private volatile int volumePct = 100;

    private AudioEngine(Context context) {
        appContext = context.getApplicationContext();
        prefs = appContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
        latencyMs = clampLatency(prefs.getInt("latency_ms", 60));
        volumePct = Math.max(0, Math.min(100, prefs.getInt("volume_pct", 100)));
        addLog("Готово к работе");
    }

    public static AudioEngine get(Context context) {
        AudioEngine local = instance;
        if (local == null) {
            synchronized (AudioEngine.class) {
                local = instance;
                if (local == null) {
                    local = new AudioEngine(context);
                    instance = local;
                }
            }
        }
        return local;
    }

    // ------------------------------------------------------------------ listeners

    public void addListener(Listener listener) {
        if (listener == null) {
            return;
        }
        listeners.addIfAbsent(listener);
        final State s = state;
        final String msg = stateMessage;
        final List<ServerInfo> list = getServers();
        final Stats st = stats;
        final boolean mu = muted;
        final boolean ms = muteSupported;
        final String logText = getLogText();
        mainHandler.post(() -> {
            listener.onState(s, msg);
            listener.onServers(list);
            listener.onStats(st);
            listener.onMute(mu, ms);
            listener.onLog(logText);
        });
    }

    public void removeListener(Listener listener) {
        listeners.remove(listener);
    }

    // ------------------------------------------------------------------ discovery

    public void startDiscovery() {
        synchronized (discoveryLock) {
            discoveryWanted = true;
            if (state == State.CONNECTING || state == State.STREAMING) {
                return;
            }
            if (discoveryThread != null) {
                return;
            }
            try {
                DatagramSocket socket = new DatagramSocket(null);
                socket.setReuseAddress(true);
                socket.setBroadcast(true);
                try {
                    socket.bind(new InetSocketAddress(Protocol.DISCOVERY_PORT));
                } catch (SocketException portBusy) {
                    socket.bind(new InetSocketAddress(0));
                    addLog("Порт поиска занят, использую резервный");
                }
                socket.setSoTimeout(700);
                try {
                    socket.setReceiveBufferSize(256 * 1024);
                } catch (SocketException ignored) {
                }
                discoverySocket = socket;
                acquireMulticastLock();
                Thread thread = new Thread(() -> discoveryLoop(socket), "pcsound-discovery");
                thread.setDaemon(true);
                discoveryThread = thread;
                thread.start();
                setState(State.SEARCHING, "Ищу компьютер в локальной сети…");
            } catch (Exception e) {
                setState(State.ERROR, "Не удалось запустить поиск: " + shortError(e));
            }
        }
    }

    public void stopDiscovery() {
        synchronized (discoveryLock) {
            discoveryWanted = false;
            stopDiscoveryLocked(true);
        }
    }

    private void stopDiscoveryForConnect() {
        synchronized (discoveryLock) {
            stopDiscoveryLocked(false);
        }
    }

    private void stopDiscoveryLocked(boolean updateState) {
        DatagramSocket socket = discoverySocket;
        discoverySocket = null;
        discoveryThread = null;
        if (socket != null) {
            try {
                socket.close();
            } catch (Exception ignored) {
            }
        }
        releaseMulticastLock();
        if (updateState && state == State.SEARCHING) {
            setState(State.IDLE, "");
        }
    }

    private void discoveryLoop(DatagramSocket socket) {
        byte[] buffer = new byte[2048];
        long lastSend = 0;
        long lastExpire = 0;
        while (discoveryWanted && !socket.isClosed()) {
            long now = SystemClock.elapsedRealtime();
            if (now - lastSend >= 1200) {
                sendDiscovery(socket);
                lastSend = now;
            }
            try {
                DatagramPacket packet = new DatagramPacket(buffer, buffer.length);
                socket.receive(packet);
                handleServerPacket(packet, now);
            } catch (SocketTimeoutException ignored) {
            } catch (Exception e) {
                if (!socket.isClosed()) {
                    addLog("Поиск: " + shortError(e));
                }
                break;
            }
            if (now - lastExpire >= 1000) {
                expireServers(now);
                lastExpire = now;
            }
        }
        synchronized (discoveryLock) {
            if (discoverySocket == socket) {
                discoverySocket = null;
                discoveryThread = null;
                releaseMulticastLock();
            }
        }
    }

    private void sendDiscovery(DatagramSocket socket) {
        try {
            byte[] payload = Protocol.utf8(Protocol.discover());
            for (InetAddress address : Protocol.broadcastAddresses()) {
                try {
                    socket.send(new DatagramPacket(payload, payload.length,
                            address, Protocol.CONTROL_PORT));
                } catch (IOException ignored) {
                }
            }
        } catch (Exception e) {
            addLog("Не удалось отправить запрос поиска: " + shortError(e));
        }
    }

    private void handleServerPacket(DatagramPacket packet, long now) {
        try {
            String text = new String(packet.getData(), packet.getOffset(),
                    packet.getLength(), StandardCharsets.UTF_8);
            JSONObject obj = new JSONObject(text);
            if (!Protocol.MAGIC_SERVER.equals(obj.optString("magic"))) {
                return;
            }
            String host = packet.getAddress().getHostAddress();
            String id = obj.optString("id", "");
            String name = obj.optString("name", host);
            int port = obj.optInt("port", Protocol.CONTROL_PORT);
            ServerInfo info = new ServerInfo(id, name, host, port, now);
            boolean isNew;
            synchronized (serversLock) {
                String oldKey = findServerKeyByIdLocked(id, host);
                isNew = oldKey == null;
                if (oldKey != null) {
                    servers.remove(oldKey);
                }
                servers.put(info.key(), info);
            }
            if (isNew) {
                addLog("Найден ПК: " + name + " (" + host + ")");
            }
            publishServers(false);
        } catch (Exception ignored) {
            // Not our packet.
        }
    }

    private String findServerKeyByIdLocked(String id, String host) {
        for (Map.Entry<String, ServerInfo> e : servers.entrySet()) {
            if (!id.isEmpty() && id.equals(e.getValue().id)) {
                return e.getKey();
            }
            if (id.isEmpty() && host.equals(e.getValue().host)) {
                return e.getKey();
            }
        }
        return null;
    }

    private void expireServers(long now) {
        boolean changed = false;
        synchronized (serversLock) {
            Iterator<Map.Entry<String, ServerInfo>> it = servers.entrySet().iterator();
            while (it.hasNext()) {
                ServerInfo info = it.next().getValue();
                if (now - info.lastSeenMs > SERVER_TIMEOUT_MS) {
                    it.remove();
                    changed = true;
                }
            }
        }
        if (changed) {
            publishServers(false);
        }
    }

    private void publishServers(boolean force) {
        List<ServerInfo> list;
        StringBuilder signature = new StringBuilder();
        synchronized (serversLock) {
            list = new ArrayList<>(servers.values());
            list.sort((a, b) -> a.name.compareToIgnoreCase(b.name));
            for (ServerInfo s : list) {
                signature.append(s.key()).append(';');
            }
        }
        String sig = signature.toString();
        if (!force && sig.equals(serversSignature)) {
            return;
        }
        serversSignature = sig;
        final List<ServerInfo> copy = Collections.unmodifiableList(list);
        mainHandler.post(() -> {
            for (Listener l : listeners) {
                l.onServers(copy);
            }
        });
    }

    public List<ServerInfo> getServers() {
        synchronized (serversLock) {
            List<ServerInfo> list = new ArrayList<>(servers.values());
            list.sort((a, b) -> a.name.compareToIgnoreCase(b.name));
            return Collections.unmodifiableList(list);
        }
    }

    private void acquireMulticastLock() {
        try {
            WifiManager wifi = (WifiManager) appContext.getSystemService(Context.WIFI_SERVICE);
            if (wifi != null) {
                multicastLock = wifi.createMulticastLock("pcsound-discovery");
                multicastLock.setReferenceCounted(false);
                multicastLock.acquire();
            }
        } catch (Exception ignored) {
        }
    }

    private void releaseMulticastLock() {
        try {
            if (multicastLock != null) {
                if (multicastLock.isHeld()) {
                    multicastLock.release();
                }
                multicastLock = null;
            }
        } catch (Exception ignored) {
        }
    }

    // ------------------------------------------------------------------ connection

    public void connect(ServerInfo server) {
        if (server == null) {
            return;
        }
        synchronized (sessionLock) {
            if (state == State.CONNECTING || state == State.STREAMING) {
                return;
            }
            autoConnectBlocked = false;
            stopDiscoveryForConnect();
            stopSessionInternal(false, null);
            setState(State.CONNECTING, "Подключение к " + server.name + "…");
            final int generation = ++connectGeneration;
            // DNS resolution and the first packet must not run on the UI thread.
            startThread("pcsound-connect", () -> establishConnection(server, generation));
        }
    }

    private void establishConnection(ServerInfo server, int generation) {
        InetAddress address;
        try {
            address = InetAddress.getByName(server.host);
        } catch (Exception e) {
            failConnect("Не удалось найти адрес " + server.host);
            return;
        }

        Session session = new Session(server, address);
        try {
            session.control = openUdpSocket(256 * 1024);
            session.media = openUdpSocket(2 * 1024 * 1024);
        } catch (Exception e) {
            closeQuietly(session.control);
            closeQuietly(session.media);
            failConnect("Не удалось открыть сетевые порты: " + shortError(e));
            return;
        }

        synchronized (sessionLock) {
            if (state != State.CONNECTING || generation != connectGeneration) {
                // The user cancelled while we were connecting.
                closeQuietly(session.control);
                closeQuietly(session.media);
                return;
            }
            current = session;
        }

        startThread("pcsound-control", () -> controlLoop(session));
        startThread("pcsound-media", () -> mediaLoop(session));
        startThread("pcsound-player", () -> playerLoop(session));
        startThread("pcsound-ping", () -> pingLoop(session));

        try {
            sendStart(session);
        } catch (Exception e) {
            failSession(session, "Не удалось отправить запрос на ПК: " + shortError(e));
        }
    }

    private void failConnect(String message) {
        synchronized (sessionLock) {
            if (state == State.CONNECTING) {
                setState(State.ERROR, message);
            }
        }
    }

    public void disconnect() {
        autoConnectBlocked = true;
        stopSessionInternal(false, null);
        if (discoveryWanted) {
            startDiscovery();
        }
    }

    private void stopSessionInternal(boolean error, String message) {
        Session session;
        synchronized (sessionLock) {
            session = current;
            current = null;
            connectGeneration++;
            if (session != null) {
                session.running = false;
                final DatagramSocket control = session.control;
                final DatagramSocket media = session.media;
                session.control = null;
                session.media = null;
                if (control != null) {
                    startThread("pcsound-stop", () -> {
                        try {
                            sendControl(control, session.address, session.server.port,
                                    Protocol.control("stop"));
                        } catch (Exception ignored) {
                        }
                        closeQuietly(control);
                    });
                }
                closeQuietly(media);
                synchronized (session.frames) {
                    session.frames.clear();
                    session.frames.notifyAll();
                }
                releaseTrack(session);
            }
            muted = false;
            muteSupported = true;
            stats = new Stats();
            if (error) {
                setState(State.ERROR, message == null ? "Ошибка" : message);
            } else {
                setState(State.IDLE, "");
            }
            notifyMute();
            notifyStats();
        }
    }

    private void failSession(Session session, String message) {
        if (current != session) {
            return;
        }
        addLog(message);
        stopSessionInternal(true, message);
        if (discoveryWanted) {
            // Let the user actually see the error for a moment, then resume
            // searching (and, if enabled, auto-reconnect).
            mainHandler.postDelayed(this::startDiscovery, 2000);
        }
    }

    private void sendStart(Session session) throws Exception {
        JSONObject obj = Protocol.control("start")
                .put("port", session.media.getLocalPort())
                .put("rate", Protocol.SAMPLE_RATE)
                .put("channels", Protocol.CHANNELS)
                .put("frameMs", Protocol.FRAME_MS);
        sendControl(session, obj);
    }

    private void sendControl(Session session, JSONObject obj) throws Exception {
        DatagramSocket socket = session.control;
        if (socket == null || socket.isClosed()) {
            throw new IOException("control socket closed");
        }
        sendControl(socket, session.address, session.server.port, obj);
    }

    private static void sendControl(DatagramSocket socket, InetAddress address,
                                    int port, JSONObject obj) throws Exception {
        byte[] data = Protocol.utf8(obj);
        socket.send(new DatagramPacket(data, data.length, address, port));
    }

    private void controlLoop(Session session) {
        byte[] buffer = new byte[8192];
        while (session.running) {
            DatagramSocket socket = session.control;
            if (socket == null) {
                break;
            }
            try {
                DatagramPacket packet = new DatagramPacket(buffer, buffer.length);
                socket.receive(packet);
                String text = new String(packet.getData(), packet.getOffset(),
                        packet.getLength(), StandardCharsets.UTF_8);
                JSONObject obj;
                try {
                    obj = new JSONObject(text);
                } catch (Exception badJson) {
                    continue;
                }
                if (!Protocol.MAGIC_CTRL.equals(obj.optString("magic"))) {
                    continue;
                }
                String cmd = obj.optString("cmd");
                if ("started".equals(cmd)) {
                    session.started = true;
                    boolean supported = obj.optBoolean("muteSupported", true);
                    session.muteSupported = supported;
                    setMuteState(obj.optBoolean("muted", false), supported);
                } else if ("pong".equals(cmd)) {
                    long sent = obj.optLong("t", 0L);
                    if (sent > 0) {
                        session.rttMs = Math.max(0, SystemClock.elapsedRealtime() - sent);
                    }
                } else if ("mute_state".equals(cmd)) {
                    boolean supported = obj.optBoolean("supported", true);
                    session.muteSupported = supported;
                    setMuteState(obj.optBoolean("value", false), supported);
                } else if ("error".equals(cmd)) {
                    failSession(session, obj.optString("message", "Ошибка на компьютере"));
                }
            } catch (SocketTimeoutException ignored) {
            } catch (Exception e) {
                if (session.running) {
                    Log.d(TAG, "control loop: " + e);
                }
                break;
            }
        }
    }

    private void mediaLoop(Session session) {
        byte[] buffer = new byte[65536];
        Map<Integer, Assembly> assemblies = new HashMap<>();
        int packetCounter = 0;
        while (session.running) {
            DatagramSocket socket = session.media;
            if (socket == null) {
                break;
            }
            try {
                DatagramPacket packet = new DatagramPacket(buffer, buffer.length);
                socket.receive(packet);
                long now = SystemClock.elapsedRealtime();
                session.lastPacketMs = now;
                Protocol.Header header = Protocol.parseHeader(packet.getData(),
                        packet.getOffset(), packet.getLength());
                if (header == null) {
                    continue;
                }
                int payloadLength = packet.getLength() - Protocol.HEADER_BYTES;
                int frameOffset = header.chunkIndex * Protocol.CHUNK_BYTES;
                if (payloadLength <= 0 || frameOffset >= header.frameBytes) {
                    continue;
                }
                payloadLength = Math.min(payloadLength, header.frameBytes - frameOffset);

                synchronized (session.frames) {
                    Assembly assembly = assemblies.get(header.seq);
                    if (assembly == null) {
                        assembly = new Assembly(header.seq, header.frameBytes, header.chunkCount);
                        assemblies.put(header.seq, assembly);
                    }
                    if (assembly.frameBytes != header.frameBytes) {
                        continue;
                    }
                    boolean complete = assembly.put(header.chunkIndex, frameOffset,
                            packet.getData(), packet.getOffset() + Protocol.HEADER_BYTES,
                            payloadLength);
                    if (complete) {
                        assemblies.remove(header.seq);
                        session.frames.put(header.seq, assembly.data);
                        while (session.frames.size() > 120) {
                            session.frames.pollFirstEntry();
                        }
                        session.packets++;
                        session.frames.notifyAll();
                        if (session.running && current == session
                                && state == State.CONNECTING) {
                            setState(State.STREAMING, "Звук идёт с ПК");
                        }
                    }
                }

                packetCounter++;
                if ((packetCounter & 127) == 0) {
                    expireAssemblies(assemblies, now);
                }
            } catch (SocketTimeoutException ignored) {
            } catch (Exception e) {
                if (session.running) {
                    Log.d(TAG, "media loop: " + e);
                }
                break;
            }
        }
    }

    private void expireAssemblies(Map<Integer, Assembly> assemblies, long now) {
        Iterator<Map.Entry<Integer, Assembly>> it = assemblies.entrySet().iterator();
        while (it.hasNext()) {
            Assembly a = it.next().getValue();
            if (now - a.createdMs > 600) {
                it.remove();
            }
        }
        if (assemblies.size() > 256) {
            assemblies.clear();
        }
    }

    private void playerLoop(Session session) {
        AudioTrack track = null;
        byte[] silence = new byte[Protocol.BYTES_PER_FRAME];
        try {
            int minBuffer = AudioTrack.getMinBufferSize(Protocol.SAMPLE_RATE,
                    AudioFormat.CHANNEL_OUT_STEREO, AudioFormat.ENCODING_PCM_16BIT);
            int bufferSize = Math.max(minBuffer > 0 ? minBuffer : 0,
                    Protocol.BYTES_PER_FRAME * 4);

            AudioAttributes attributes = new AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                    .build();
            AudioFormat format = new AudioFormat.Builder()
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                    .setSampleRate(Protocol.SAMPLE_RATE)
                    .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                    .build();

            AudioTrack.Builder builder = new AudioTrack.Builder()
                    .setAudioAttributes(attributes)
                    .setAudioFormat(format)
                    .setTransferMode(AudioTrack.MODE_STREAM)
                    .setBufferSizeInBytes(bufferSize);
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                builder.setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY);
            }
            track = builder.build();
            session.track = track;
            if (!session.running) {
                return;
            }
            track.setVolume(volumePct / 100f);
            track.play();

            // Pre-buffer a few frames before the first write.
            synchronized (session.frames) {
                long deadline = SystemClock.elapsedRealtime() + 3000;
                while (session.running && session.frames.size() < session.prebufferFrames) {
                    long remaining = deadline - SystemClock.elapsedRealtime();
                    if (remaining <= 0) {
                        break;
                    }
                    session.frames.wait(Math.min(50, remaining));
                }
                // While the AudioTrack was being created the server kept
                // sending; drop the surplus so we start at the target latency
                // instead of carrying the whole startup burst.
                while (session.frames.size() > session.prebufferFrames
                        && !session.frames.isEmpty()) {
                    session.frames.pollFirstEntry();
                }
                session.nextSeq = -1;
                session.frames.notifyAll();
            }

            long nextStats = 0;
            long lastAdjust = 0;
            int holdFrames = 0;
            while (session.running) {
                long now = SystemClock.elapsedRealtime();
                int targetFrames = Math.max(2, latencyMs / Protocol.FRAME_MS);

                // сумма-контроль буфера раз в полсекунды: держим наполнение
                // рядом с выбранным пользователем значением, чтобы задержка
                // не «уплывала» со временем от тактовой разницы и рывков сети
                if (now - lastAdjust >= ADJUST_INTERVAL_MS) {
                    lastAdjust = now;
                    int fill;
                    synchronized (session.frames) {
                        fill = session.frames.size();
                    }
                    if (holdFrames == 0 && fill > targetFrames + 8) {
                        // Сбрасываем лишнее сразу после длинного обрыва сети
                        session.droppedFrames += dropOldest(session,
                                fill - (targetFrames + 2));
                    } else if (holdFrames == 0 && fill > targetFrames + 2) {
                        session.droppedFrames += dropOldest(session,
                                Math.min(3, fill - (targetFrames + 2)));
                    } else if (holdFrames == 0 && fill < targetFrames - 2) {
                        // не потребляем пару кадров — очередь догоняет
                        holdFrames = Math.min(6, targetFrames - fill);
                    }
                }

                byte[] frame;
                if (holdFrames > 0) {
                    frame = silence;
                    holdFrames--;
                    session.silenceFrames++;
                } else {
                    frame = takeFrame(session);
                    if (frame == null) {
                        frame = silence;
                        session.silenceFrames++;
                    }
                }
                int written = track.write(frame, 0, frame.length, AudioTrack.WRITE_BLOCKING);
                if (written < 0) {
                    throw new IOException("AudioTrack.write=" + written);
                }
                if (frame != silence) {
                    session.playedFrames++;
                }

                if (now >= nextStats) {
                    nextStats = now + 1000;
                    publishStats(session);
                }
            }
        } catch (Exception e) {
            if (session.running) {
                failSession(session, "Ошибка аудиовыхода: " + shortError(e));
            }
        } finally {
            if (session.track == track) {
                session.track = null;
            }
            releaseTrack(track);
        }
    }

    private static int dropOldest(Session session, int count) {
        if (count <= 0) {
            return 0;
        }
        int dropped = 0;
        synchronized (session.frames) {
            for (int i = 0; i < count; i++) {
                Map.Entry<Integer, byte[]> entry = session.frames.pollFirstEntry();
                if (entry == null) {
                    break;
                }
                session.nextSeq = entry.getKey() + 1;
                dropped++;
            }
        }
        return dropped;
    }

    private byte[] takeFrame(Session session) {
        synchronized (session.frames) {
            if (session.frames.isEmpty()) {
                return null;
            }
            // Drop frames that were already skipped.
            while (!session.frames.isEmpty() && session.frames.firstKey() < session.nextSeq) {
                session.frames.pollFirstEntry();
            }
            if (session.frames.isEmpty()) {
                return null;
            }
            int first = session.frames.firstKey();
            if (session.nextSeq < 0) {
                session.nextSeq = first;
            }
            byte[] frame = session.frames.remove(session.nextSeq);
            if (frame != null) {
                session.nextSeq++;
                return frame;
            }
            // Packet(s) lost: jump to the next available frame instead of
            // adding delay. Each frame is only 10 ms, so this is inaudible.
            int lost = Math.max(0, first - session.nextSeq);
            session.lostFrames += lost;
            session.nextSeq = first + 1;
            return session.frames.remove(first);
        }
    }

    private void pingLoop(Session session) {
        while (session.running) {
            long now = SystemClock.elapsedRealtime();
            try {
                if (!session.started) {
                    if (session.startRetries == 0
                            || (now - session.createdMs > 1200 && session.startRetries < 3)) {
                        session.startRetries++;
                        sendStart(session);
                    }
                    if (now - session.createdMs > CONNECT_TIMEOUT_MS) {
                        failSession(session, "ПК не отвечает. Проверьте сервер и брандмауэр.");
                        return;
                    }
                } else if (session.lastPacketMs > 0
                        && now - session.lastPacketMs > STREAM_TIMEOUT_MS) {
                    failSession(session, "Поток прерван. Проверьте Wi-Fi.");
                    return;
                } else if (session.lastPacketMs == 0
                        && now - session.createdMs > CONNECT_TIMEOUT_MS) {
                    failSession(session, "ПК подключён, но звук не приходит. "
                            + "Проверьте аудиоустройство на компьютере.");
                    return;
                }
                JSONObject ping = Protocol.control("ping").put("t", now);
                sendControl(session, ping);
            } catch (Exception e) {
                if (session.running) {
                    failSession(session, "Соединение с ПК потеряно: " + shortError(e));
                }
                return;
            }
            try {
                Thread.sleep(1500);
            } catch (InterruptedException e) {
                return;
            }
        }
    }

    private void publishStats(Session session) {
        int bufferMs;
        synchronized (session.frames) {
            int fill = session.frames.size();
            // Показываем сглаженное значение: мгновенное наполнение скачет
            // с каждым рывком сети, а среднее отражает фактическую задержку.
            if (session.fillEma <= 0) {
                session.fillEma = fill;
            } else {
                session.fillEma += 0.35f * (fill - session.fillEma);
            }
            bufferMs = Math.round(session.fillEma) * Protocol.FRAME_MS;
        }
        Stats s = new Stats(session.running, bufferMs, session.rttMs,
                session.lostFrames, session.droppedFrames, session.playedFrames);
        stats = s;
        final Stats copy = s;
        mainHandler.post(() -> {
            for (Listener l : listeners) {
                l.onStats(copy);
            }
        });
    }

    private void notifyStats() {
        final Stats copy = stats;
        mainHandler.post(() -> {
            for (Listener l : listeners) {
                l.onStats(copy);
            }
        });
    }

    // ------------------------------------------------------------------ audio settings

    public void setVolume(int percent) {
        volumePct = Math.max(0, Math.min(100, percent));
        prefs.edit().putInt("volume_pct", volumePct).apply();
        Session session = current;
        if (session != null) {
            AudioTrack track = session.track;
            if (track != null) {
                try {
                    track.setVolume(volumePct / 100f);
                } catch (Exception ignored) {
                }
            }
        }
    }

    public int getVolume() {
        return volumePct;
    }

    public void setLatency(int ms) {
        latencyMs = clampLatency(ms);
        prefs.edit().putInt("latency_ms", latencyMs).apply();
    }

    public int getLatency() {
        return latencyMs;
    }

    private static int clampLatency(int ms) {
        int clamped = Math.max(20, Math.min(200, ms));
        return (clamped / 10) * 10;
    }

    public void setMuted(boolean value) {
        Session session = current;
        if (session == null) {
            return;
        }
        if (!session.muteSupported) {
            setMuteState(false, false);
            return;
        }
        final boolean target = value;
        setMuteState(target, session.muteSupported);
        startThread("pcsound-mute", () -> {
            try {
                sendControl(session, Protocol.control("mute").put("value", target));
            } catch (Exception e) {
                addLog("Не удалось переключить звук ПК: " + shortError(e));
            }
        });
    }

    private void setMuteState(boolean value, boolean supported) {
        boolean changed = muted != value || muteSupported != supported;
        muted = value;
        muteSupported = supported;
        if (changed) {
            addLog(value ? "Динамики ПК заглушены" : "Звук на ПК включён");
            notifyMute();
        }
    }

    private void notifyMute() {
        final boolean value = muted;
        final boolean supported = muteSupported;
        mainHandler.post(() -> {
            for (Listener l : listeners) {
                l.onMute(value, supported);
            }
        });
    }

    public boolean isMuted() {
        return muted;
    }

    public boolean isMuteSupported() {
        return muteSupported;
    }

    public boolean isAutoConnectBlocked() {
        return autoConnectBlocked;
    }

    /** Адрес ПК, к которому идёт трансляция (для тачпада), иначе null. */
    public String getConnectedHost() {
        Session session = current;
        return session == null ? null : session.server.host;
    }

    public String getConnectedName() {
        Session session = current;
        return session == null ? null : session.server.name;
    }

    // ------------------------------------------------------------------ info

    public State getState() {
        return state;
    }

    public String getStateMessage() {
        return stateMessage;
    }

    public Stats getStats() {
        return stats;
    }

    public String getLogText() {
        synchronized (logLock) {
            StringBuilder sb = new StringBuilder();
            int start = Math.max(0, logLines.size() - 8);
            for (int i = start; i < logLines.size(); i++) {
                if (sb.length() > 0) {
                    sb.append('\n');
                }
                sb.append(logLines.get(i));
            }
            return sb.toString();
        }
    }

    private void addLog(String message) {
        String time = new SimpleDateFormat("HH:mm:ss", Locale.getDefault()).format(new Date());
        String line = time + "  " + message;
        Log.i(TAG, message);
        synchronized (logLock) {
            logLines.add(line);
            while (logLines.size() > 40) {
                logLines.remove(0);
            }
        }
        final String full = getLogText();
        mainHandler.post(() -> {
            for (Listener l : listeners) {
                l.onLog(full);
            }
        });
    }

    private void setState(State newState, String message) {
        state = newState;
        stateMessage = message == null ? "" : message;
        final State s = state;
        final String m = stateMessage;
        mainHandler.post(() -> {
            for (Listener l : listeners) {
                l.onState(s, m);
            }
        });
    }

    // ------------------------------------------------------------------ helpers

    private static DatagramSocket openUdpSocket(int receiveBuffer) throws SocketException {
        DatagramSocket socket = new DatagramSocket(null);
        socket.setReuseAddress(true);
        try {
            socket.setReceiveBufferSize(receiveBuffer);
        } catch (SocketException ignored) {
        }
        socket.bind(new InetSocketAddress(0));
        socket.setSoTimeout(700);
        return socket;
    }

    private static void startThread(String name, Runnable runnable) {
        Thread thread = new Thread(runnable, name);
        thread.setDaemon(true);
        thread.start();
    }

    private static void closeQuietly(DatagramSocket socket) {
        if (socket != null) {
            try {
                socket.close();
            } catch (Exception ignored) {
            }
        }
    }

    private static void releaseTrack(Session session) {
        if (session != null) {
            AudioTrack track = session.track;
            session.track = null;
            releaseTrack(track);
        }
    }

    private static void releaseTrack(AudioTrack track) {
        if (track == null) {
            return;
        }
        try {
            if (track.getPlayState() != AudioTrack.PLAYSTATE_STOPPED) {
                track.stop();
            }
        } catch (Exception ignored) {
        }
        try {
            track.release();
        } catch (Exception ignored) {
        }
    }

    private static String shortError(Throwable t) {
        String m = t.getMessage();
        if (m == null || m.isEmpty()) {
            return t.getClass().getSimpleName();
        }
        return m;
    }

    private final class Session {
        final ServerInfo server;
        final InetAddress address;
        final int prebufferFrames;
        final long createdMs = SystemClock.elapsedRealtime();
        final TreeMap<Integer, byte[]> frames = new TreeMap<>();

        volatile boolean running = true;
        volatile boolean started = false;
        volatile DatagramSocket control;
        volatile DatagramSocket media;
        volatile AudioTrack track;
        volatile long lastPacketMs = 0;
        volatile long rttMs = -1;
        volatile int packets = 0;
        volatile int lostFrames = 0;
        volatile int droppedFrames = 0;
        volatile int playedFrames = 0;
        volatile int silenceFrames = 0;
        volatile int startRetries = 0;
        volatile boolean muteSupported = true;
        volatile float fillEma = 0f;

        int nextSeq = -1;

        Session(ServerInfo server, InetAddress address) {
            this.server = server;
            this.address = address;
            this.prebufferFrames = Math.max(2,
                    Math.min(20, latencyMs / Protocol.FRAME_MS));
        }
    }

    private static final class Assembly {
        final int seq;
        final int frameBytes;
        final int chunkCount;
        final byte[] data;
        final boolean[] received;
        final long createdMs = SystemClock.elapsedRealtime();
        int receivedCount = 0;

        Assembly(int seq, int frameBytes, int chunkCount) {
            this.seq = seq;
            this.frameBytes = frameBytes;
            this.chunkCount = chunkCount;
            this.data = new byte[frameBytes];
            this.received = new boolean[chunkCount];
        }

        boolean put(int index, int offset, byte[] source, int sourceOffset, int length) {
            if (index < 0 || index >= chunkCount || received[index]) {
                return false;
            }
            int copy = Math.min(length, frameBytes - offset);
            if (copy <= 0) {
                return false;
            }
            System.arraycopy(source, sourceOffset, data, offset, copy);
            received[index] = true;
            receivedCount++;
            return receivedCount == chunkCount;
        }
    }
}
