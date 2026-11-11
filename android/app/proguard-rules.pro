# gRPC / protobuf reflection is not used by the generated Kotlin stubs.
-dontwarn com.google.protobuf.**
-keep class io.grpc.** { *; }

# Optional code paths inside grpc-java: okhttp 2.x connection specs and JNDI
# DNS lookup. Android uses gRPC's OkHttp transport and the platform resolver, so
# these classes are never loaded; R8 only needs the warnings suppressed.
-dontwarn com.squareup.okhttp.**
-dontwarn javax.naming.**
-dontwarn io.grpc.internal.JndiResourceResolverFactory
-dontwarn org.codehaus.mojo.animal_sniffer.**

# Generated protobuf/gRPC message classes are resolved by name in a few places
# (descriptors, service registry); keep them intact.
-keep class com.wheezper.v1.** { *; }
