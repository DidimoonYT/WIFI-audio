package com.pcsoundlink.app;

import android.content.Context;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.os.Handler;
import android.os.Looper;

import org.json.JSONObject;

import java.io.ByteArrayOutputStream;
import java.io.DataInputStream;
import java.io.InputStream;
import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.net.SocketTimeoutException;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * Управляет демонстрацией экрана: слушает TCP (сервер подключается сам),
 * принимает JPEG-кадры и отправляет команды мыши на ПК.
 */
public class TouchpadClient {

    public interface Listener {
        void onStatus(String message);

        void onFrame(Bitmap frame);
    }

    private static final int MAX_FRAME_BYTES = 8 * 1024 * 1024;

    private final String host;
    private final Context context;
    private final Handler main = new Handler(Looper.getMainLooper());
    private final ExecutorService sender = Executors.newSingleThreadExecutor();
    private volatile Listener listener;
    private volatile ServerSocket tcpServer;
    private volatile DatagramSocket udp;
    private volatile InetAddress address;
    private volatile boolean running = false;
    private volatile boolean started = false;
    private int tcpPort = 0;

    private Thread acceptThread;
    private Thread readThread;

    public TouchpadClient(Context context, String host) {
        this.context = context.getApplicationContext();
        this.host = host;
    }

    public int getTcpPort() {
        return tcpPort;
    }

    public boolean isStarted() {
        return started;
    }

    public void setListener(Listener l) {
        listener = l;
    }

    private void postStatus(final String message) {
        main.post(() -> {
            Listener l = listener;
            if (l != null) {
                l.onStatus(message);
            }
        });
    }

    private void postFrame(final Bitmap bitmap) {
        main.post(() -> {
            Listener l = listener;
            if (l != null) {
                l.onFrame(bitmap);
            }
        });
    }

    public void start() {
        if (running) {
            return;
        }
        running = true;
        acceptThread = new Thread(this::acceptLoop, "pcsound-screen");
        acceptThread.start();
    }

    public void stop() {
        if (!running) {
            return;
        }
        running = false;
        try {
            sendControl(Protocol.control("screen_stop"));
        } catch (Exception ignored) {
        }
        closeQuietly(tcpServer);
        closeQuietly(udp);
        sender.shutdownNow();
        if (acceptThread != null) {
            acceptThread.interrupt();
        }
        if (readThread != null) {
            readThread.interrupt();
        }
    }

    private void closeQuietly(java.io.Closeable c) {
        try {
            if (c != null) {
                c.close();
            }
        } catch (Exception ignored) {
        }
    }

    private void acceptLoop() {
        while (running) {
            try {
                address = InetAddress.getByName(host);
                if (udp == null || udp.isClosed()) {
                    udp = new DatagramSocket();
                }
                ServerSocket server = new ServerSocket(0, 1,
                        InetAddress.getByName("0.0.0.0"));
                tcpServer = server;
                tcpPort = server.getLocalPort();

                sendControl(Protocol.control("screen_start").put("port", tcpPort));
                started = true;
                postStatus(context.getString(R.string.touchpad_status_starting));

                server.setSoTimeout(8000);
                Socket socket = null;
                while (running && socket == null) {
                    try {
                        socket = server.accept();
                    } catch (SocketTimeoutException e) {
                        // Заставляем сервер ещё раз подключиться.
                        sendControl(Protocol.control("screen_start").put("port", tcpPort));
                    }
                }
                if (!running) {
                    closeQuietly(server);
                    return;
                }
                server.close();
                tcpServer = null;
                postStatus(context.getString(R.string.touchpad_status_streaming));
                readFrames(socket);
                closeQuietly(socket);
                if (running) {
                    postStatus(context.getString(R.string.touchpad_status_reconnecting));
                    Thread.sleep(1000);
                }
            } catch (InterruptedException e) {
                return;
            } catch (Exception e) {
                if (!running) {
                    return;
                }
                postStatus(context.getString(R.string.touchpad_error, shortError(e)));
                try {
                    Thread.sleep(2000);
                } catch (InterruptedException ie) {
                    return;
                }
            }
        }
    }

    private void readFrames(Socket socket) {
        try {
            socket.setTcpNoDelay(true);
            InputStream in = socket.getInputStream();
            DataInputStream data = new DataInputStream(in);
            byte[] lenBuf = new byte[4];
            while (running) {
                if (!readFully(data, lenBuf, 4)) {
                    break;
                }
                int length = (lenBuf[0] & 0xFF) | (lenBuf[1] & 0xFF) << 8
                        | (lenBuf[2] & 0xFF) << 16 | (lenBuf[3] & 0xFF) << 24;
                if (length <= 0 || length > MAX_FRAME_BYTES) {
                    break;
                }
                byte[] jpeg = new byte[length];
                if (!readFully(data, jpeg, length)) {
                    break;
                }
                Bitmap bitmap = BitmapFactory.decodeByteArray(jpeg, 0, length);
                if (bitmap != null) {
                    postFrame(bitmap);
                }
            }
        } catch (Exception e) {
            if (running) {
                postStatus(context.getString(R.string.touchpad_status_lost));
            }
        } finally {
            closeQuietly(socket);
        }
    }

    private static boolean readFully(DataInputStream in, byte[] buf, int len)
            throws Exception {
        int read = 0;
        while (read < len) {
            int n = in.read(buf, read, len - read);
            if (n < 0) {
                return false;
            }
            read += n;
        }
        return true;
    }

    private void sendControl(JSONObject obj) throws Exception {
        DatagramSocket socket = udp;
        InetAddress addr = address;
        if (socket == null || socket.isClosed() || addr == null) {
            return;
        }
        byte[] data = Protocol.utf8(obj);
        try {
            socket.send(new DatagramPacket(data, data.length, addr, Protocol.CONTROL_PORT));
        } catch (Exception e) {
            throw e;
        }
    }

    /** Вызывается из потока касаний — отправка не должна блокировать UI. */
    public void sendMouse(String action, String button, boolean down, float dx,
                          float dy) {
        try {
            JSONObject obj = Protocol.control("mouse").put("action", action);
            if (button != null) {
                obj.put("button", button);
            }
            if (action.equals("button")) {
                obj.put("down", down);
            }
            if (action.equals("move")) {
                obj.put("dx", (int) dx);
                obj.put("dy", (int) dy);
            }
            if (action.equals("wheel")) {
                obj.put("by", (int) dy);
            }
            android.util.Log.d("TPtouch", "sendMouse " + obj);
            final JSONObject toSend = obj;
            sender.execute(() -> {
                try {
                    sendControl(toSend);
                } catch (Exception e) {
                    android.util.Log.e("TPtouch", "sendMouse failed: " + e);
                }
            });
        } catch (Exception e) {
            android.util.Log.e("TPtouch", "sendMouse failed: " + e);
        }
    }

    /** Отправляет текст, набранный на телефоне, на ПК. */
    public void sendText(String text) {
        if (text == null || text.isEmpty()) {
            return;
        }
        try {
            final JSONObject obj = Protocol.control("text").put("text", text);
            sender.execute(() -> {
                try {
                    sendControl(obj);
                } catch (Exception e) {
                    android.util.Log.e("TPtouch", "sendText failed: " + e);
                }
            });
        } catch (Exception e) {
            android.util.Log.e("TPtouch", "sendText failed: " + e);
        }
    }

    public void sendMouse(BufferedMotion motion) {
        try {
            final JSONObject obj = Protocol.control("mouse")
                    .put("action", "move")
                    .put("dx", (int) motion.dx)
                    .put("dy", (int) motion.dy);
            sender.execute(() -> {
                try {
                    sendControl(obj);
                } catch (Exception e) {
                    android.util.Log.e("TPtouch", "sendMouse failed: " + e);
                }
            });
        } catch (Exception e) {
            android.util.Log.e("TPtouch", "sendMouse failed: " + e);
        }
    }

    public static final class BufferedMotion {
        public float dx;
        public float dy;
    }

    private static String shortError(Throwable t) {
        String m = t.getMessage();
        return m == null || m.isEmpty() ? t.getClass().getSimpleName() : m;
    }
}
