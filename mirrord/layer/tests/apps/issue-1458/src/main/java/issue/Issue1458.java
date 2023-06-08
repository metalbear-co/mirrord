/*
 * Copyright 2012 The Netty Project
 *
 * The Netty Project licenses this file to you under the Apache License,
 * version 2.0 (the "License"); you may not use this file except in compliance
 * with the License. You may obtain a copy of the License at:
 *
 *   https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS, WITHOUT
 * WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the
 * License for the specific language governing permissions and limitations
 * under the License.
 */
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
 */
public final class Issue1054 {
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
