package com.pcsoundlink.app;

import java.util.Objects;

/** A PC discovered on the local network (or entered manually). */
public final class ServerInfo {
    public final String id;
    public final String name;
    public final String host;
    public final int port;
    public final long lastSeenMs;

    public ServerInfo(String id, String name, String host, int port, long lastSeenMs) {
        this.id = id == null ? "" : id;
        this.name = name == null || name.isEmpty() ? host : name;
        this.host = host;
        this.port = port > 0 ? port : Protocol.CONTROL_PORT;
        this.lastSeenMs = lastSeenMs;
    }

    public String key() {
        return (id.isEmpty() ? host : id) + "|" + host + "|" + port;
    }

    public String displayName() {
        return name + " — " + host;
    }

    @Override
    public boolean equals(Object o) {
        if (this == o) return true;
        if (!(o instanceof ServerInfo)) return false;
        ServerInfo that = (ServerInfo) o;
        return port == that.port && id.equals(that.id) && name.equals(that.name)
                && host.equals(that.host);
    }

    @Override
    public int hashCode() {
        return Objects.hash(id, name, host, port);
    }
}
