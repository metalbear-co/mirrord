package issue;

import java.net.InetAddress;
import io.netty.channel.EventLoopGroup;
import io.netty.channel.ReflectiveChannelFactory;
import io.netty.channel.nio.NioEventLoopGroup;
import io.netty.channel.socket.nio.NioDatagramChannel;
import io.netty.resolver.dns.DnsNameResolver;
import io.netty.resolver.dns.DnsNameResolverBuilder;

/*
 * Test netty DNS resolves with UDP (relevant for macos).
 *
 * Issue: https://github.com/metalbear-co/mirrord/issues/1458
 *
 * How to build this:
 *
 * ```sh
 * mvn clean package assembly:single
 * ```
 *
 * How to run:
 *
 * ```sh
 * java -jar target/issue1458-1.0-jar-with-dependencies.jar
 * ```
 *
 */
public final class Issue1458 {
    public static void main(String[] args) throws Exception {
        System.console().printf("test issue 1458: START\n");

        EventLoopGroup dnsGroup = new NioEventLoopGroup();
        ReflectiveChannelFactory<NioDatagramChannel> dnsFactory = new ReflectiveChannelFactory<NioDatagramChannel>(
                NioDatagramChannel.class);
        try {
            final DnsNameResolver dns = new DnsNameResolverBuilder().eventLoop(dnsGroup.next())
                    .channelFactory(dnsFactory).build();
            InetAddress address = dns.resolve("www.google.com").sync().await().get();

            System.console().printf("resolved address %s\n", address);
            System.console().printf("test issue 1458: SUCCESS\n");

        } finally {
            dnsGroup.shutdownGracefully();
        }
    }
}
