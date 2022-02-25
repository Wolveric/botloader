import { Commands } from 'botloader';
import { runOnce, sendScriptCompletion } from 'lib';

script.createCommand(
    Commands.slashCommand("gaming", "this is a gaming command")
        .addOptionNumber("amount", "amount of gaming")
        .addOptionString("what", "what to game", { autocomplete: gamingAutocomplete })
        .build((ctx, args) => {
            // stuff here
            let a = args.amount;
            ctx.sendResponse(`we are gaming: ${a}`);
        })
);


function gamingAutocomplete(data: {}) {
    return [{
        name: "lol",
        value: "lol",
    }, {
        name: "lost ark",
        value: "loast_ark",
    }]
}

script.createCommand(
    Commands.userCommand("throw", "throw this user up in the air")
        .build((ctx, target) => {
            // stuff here
            ctx.sendResponse(`throwing ${target.user.id}`);
        })
);


script.createCommand(
    Commands.messageCommand("report", "report this message")
        .build((ctx, target) => {
            // stuff here
            ctx.sendResponse(`reporing ${target.id} made by ${target.author.id}`);
        })
);

runOnce("commands.ts", () => {
    sendScriptCompletion();
});