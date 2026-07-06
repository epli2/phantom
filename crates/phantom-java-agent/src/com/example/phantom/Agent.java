package com.example.phantom;

import java.lang.instrument.Instrumentation;
import java.net.*;
import java.util.Collections;
import java.util.List;
import java.io.IOException;
import javax.net.ssl.*;
import java.security.SecureRandom;
import java.security.Security;
import java.security.cert.X509Certificate;

public class Agent {
    public static void premain(String agentArgs, Instrumentation inst) {
        String proxyHost = System.getProperty("http.proxyHost", "127.0.0.1");
        int proxyPort = Integer.getInteger("http.proxyPort", 8080);

        System.err.println("phantom-agent: Initializing Java Agent...");
        System.err.println("phantom-agent: Forcing proxy -> " + proxyHost + ":" + proxyPort);

        // 1. Force global ProxySelector
        final Proxy phantomProxy = new Proxy(Proxy.Type.HTTP, new InetSocketAddress(proxyHost, proxyPort));
        ProxySelector.setDefault(new ProxySelector() {
            @Override
            public List<Proxy> select(URI uri) {
                return Collections.singletonList(phantomProxy);
            }
            @Override
            public void connectFailed(URI uri, SocketAddress sa, IOException ioe) {}
        });

        System.setProperty("http.nonProxyHosts", "");
        System.setProperty("https.nonProxyHosts", "");

        // 2. Disable SSL Verification (Trust All)
        try {
            TrustManager[] trustAllCerts = new TrustManager[]{
                new X509TrustManager() {
                    public X509Certificate[] getAcceptedIssuers() { return new X509Certificate[0]; }
                    public void checkClientTrusted(X509Certificate[] certs, String authType) {}
                    public void checkServerTrusted(X509Certificate[] certs, String authType) {}
                }
            };

            SSLContext sc = SSLContext.getInstance("TLS");
            sc.init(null, trustAllCerts, new SecureRandom());
            SSLContext.setDefault(sc);
            HttpsURLConnection.setDefaultSSLSocketFactory(sc.getSocketFactory());
            HttpsURLConnection.setDefaultHostnameVerifier((hostname, session) -> true);
            
            // For Netty/Jetty/etc. - Try to influence the default TrustManager
            // Note: This is a bit of a hack without bytecode manipulation
            System.setProperty("com.sun.net.ssl.checkRevocation", "false");
            System.setProperty("jdk.tls.allowUnsafeServerCertificates", "true");
            
            System.err.println("phantom-agent: SSL verification disabled (trust-all).");
        } catch (Exception e) {
            System.err.println("phantom-agent: Failed to disable SSL verification: " + e.getMessage());
        }
    }
}
