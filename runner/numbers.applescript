on run argv
    set sourceFile to (POSIX file (item 1 of argv)) as alias
    set destinationFile to POSIX file (item 2 of argv)
    set operation to item 3 of argv
    with timeout of 50 seconds
        -- Cold launch can briefly accept the launch event before document events are ready.
        set convertedDocument to missing value
        repeat with attempt from 1 to 30
            try
                tell application "Numbers" to set convertedDocument to open sourceFile
                exit repeat
            on error messageText number messageNumber
                if messageNumber is not -600 or attempt is 30 then error messageText number messageNumber
                delay 0.2
            end try
        end repeat
        tell application "Numbers"
            try
                if operation is "open" then
                    export convertedDocument to destinationFile as Microsoft Excel with properties {exclude summary worksheet:true}
                else
                    save convertedDocument in destinationFile
                end if
                close convertedDocument saving no
            on error messageText number messageNumber
                try
                    close convertedDocument saving no
                end try
                error messageText number messageNumber
            end try
        end tell
    end timeout
end run
