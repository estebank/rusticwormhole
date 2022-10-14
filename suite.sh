cargo build --release
killall rusticwormhole

#for BUF_SIZE in 512 1024 2048 3072 4096 8192 10240 20480 41920 83840 167680
#for BUF_SIZE in 4096 8192 10240 20480 41920 83840 167680 335360 670720
#for BUF_SIZE in 167680 335360 670720 1341440 2482880
for BUF_SIZE in 0 0 0 0 0 83840 83840 83840 83840 83840
#for BUF_SIZE in 10240
#for BUF_SIZE in 0 #10240
do
    ./target/release/rusticwormhole registry &
    sleep 3

    # Always send the biggest size
    ./target/release/rusticwormhole --buf-size 0 receive a 7777 &
    sleep 3
    echo "Buffer size" $BUF_SIZE
    #command time -l ./target/release/rusticwormhole --buf-size $BUF_SIZE send b a directory
    #command time -l ./target/release/rusticwormhole --buf-size $BUF_SIZE send b a directory --threadpool-size 2
    #command time -l ./target/release/rusticwormhole --buf-size $BUF_SIZE send b a directory --threadpool-size 8
    command time -l ./target/release/rusticwormhole --buf-size $BUF_SIZE send b a myfile
    #cargo flamegraph -- --buf-size $BUF_SIZE send b a myfile
    sleep 1
    killall rusticwormhole
done

