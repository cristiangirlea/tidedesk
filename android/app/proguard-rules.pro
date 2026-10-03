# The Rust library calls back into nothing, but its external functions must
# keep their names.
-keep class app.tidedesk.viewer.Native { native <methods>; }
