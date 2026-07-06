package com.example.phantom;

import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;

import org.apache.hc.client5.http.classic.methods.HttpGet;
import org.apache.hc.client5.http.impl.classic.CloseableHttpClient;
import org.apache.hc.client5.http.impl.classic.HttpClients;
import org.apache.hc.core5.http.io.entity.EntityUtils;

import io.netty.handler.ssl.SslContext;
import io.netty.handler.ssl.SslContextBuilder;
import io.netty.handler.ssl.util.InsecureTrustManagerFactory;

import org.eclipse.jetty.client.HttpProxy;

import javax.net.ssl.*;
import java.security.cert.X509Certificate;

public class Client {

    static void runJdkHttpClient(String httpBase, String httpsBase) throws Exception {
        HttpClient client = HttpClient.newBuilder().build();
        client.send(HttpRequest.newBuilder().uri(URI.create(httpBase + "/api/health")).header("x-phantom-client", "jdk-httpclient").GET().build(), HttpResponse.BodyHandlers.ofString());
        if (httpsBase != null) {
            client.send(HttpRequest.newBuilder().uri(URI.create(httpsBase + "/api/health")).header("x-phantom-client", "jdk-httpclient").GET().build(), HttpResponse.BodyHandlers.ofString());
        }
    }

    static void runApacheHttpClient(String httpBase, String httpsBase) throws Exception {
        try (CloseableHttpClient client = HttpClients.createSystem()) {
            HttpGet get1 = new HttpGet(httpBase + "/api/health");
            get1.addHeader("x-phantom-client", "apache-httpclient");
            client.execute(get1, response -> {
                EntityUtils.consume(response.getEntity());
                return null;
            });
            if (httpsBase != null) {
                HttpGet get2 = new HttpGet(httpsBase + "/api/health");
                get2.addHeader("x-phantom-client", "apache-httpclient");
                client.execute(get2, response -> {
                    EntityUtils.consume(response.getEntity());
                    return null;
                });
            }
        }
    }

    static void runNettyHttpClient(String httpBase, String httpsBase) throws Exception {
        SslContext sslContext = SslContextBuilder.forClient().trustManager(InsecureTrustManagerFactory.INSTANCE).build();
        reactor.netty.http.client.HttpClient client = reactor.netty.http.client.HttpClient.create()
                .secure(spec -> spec.sslContext(sslContext))
                .proxyWithSystemProperties();
        client.headers(h -> h.add("x-phantom-client", "netty-httpclient")).get().uri(httpBase + "/api/health").response().block();
        if (httpsBase != null) {
            client.headers(h -> h.add("x-phantom-client", "netty-httpclient")).get().uri(httpsBase + "/api/health").response().block();
        }
    }

    static void runJettyHttpClient(String httpBase, String httpsBase) throws Exception {
        org.eclipse.jetty.client.HttpClient client = new org.eclipse.jetty.client.HttpClient();
        String proxyHost = System.getProperty("http.proxyHost", "127.0.0.1");
        int proxyPort = Integer.getInteger("http.proxyPort", 8080);
        client.getProxyConfiguration().getProxies().add(new HttpProxy(proxyHost, proxyPort));
        client.start();
        try {
            client.newRequest(httpBase + "/api/health").header("x-phantom-client", "jetty-httpclient").send();
        } finally {
            client.stop();
        }
    }

    static void runOkHttpClient(String httpBase, String httpsBase) throws Exception {
        // For OkHttp, we need to explicitly trust all certs if we're not using bytecode hooking.
        TrustManager[] trustAllCerts = new TrustManager[]{
            new X509TrustManager() {
                public void checkClientTrusted(X509Certificate[] chain, String authType) {}
                public void checkServerTrusted(X509Certificate[] chain, String authType) {}
                public X509Certificate[] getAcceptedIssuers() { return new X509Certificate[]{}; }
            }
        };
        SSLContext sslContext = SSLContext.getInstance("SSL");
        sslContext.init(null, trustAllCerts, new java.security.SecureRandom());

        okhttp3.OkHttpClient client = new okhttp3.OkHttpClient.Builder()
                .sslSocketFactory(sslContext.getSocketFactory(), (X509TrustManager)trustAllCerts[0])
                .hostnameVerifier((hostname, session) -> true)
                .build();

        // HTTP
        okhttp3.Request req1 = new okhttp3.Request.Builder().url(httpBase + "/api/health").header("x-phantom-client", "okhttp-httpclient").build();
        try (okhttp3.Response resp1 = client.newCall(req1).execute()) {
            EntityUtils.consume(null); // Just closing
        }

        // HTTPS
        if (httpsBase != null) {
            okhttp3.Request req2 = new okhttp3.Request.Builder().url(httpsBase + "/api/health").header("x-phantom-client", "okhttp-httpclient").build();
            try (okhttp3.Response resp2 = client.newCall(req2).execute()) {
                EntityUtils.consume(null);
            }
        }
    }

    public static void main(String[] args) throws Exception {
        String httpBase = System.getenv("BACKEND_HTTP_URL");
        String httpsBase = System.getenv("BACKEND_HTTPS_URL");
        runJdkHttpClient(httpBase, httpsBase);
        runApacheHttpClient(httpBase, httpsBase);
        runNettyHttpClient(httpBase, httpsBase);
        runJettyHttpClient(httpBase, httpsBase);
        runOkHttpClient(httpBase, httpsBase);
        System.out.println("CLIENT_DONE");
    }
}
